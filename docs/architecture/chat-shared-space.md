# Chat shared-space increment

Chat is the persistent Tilo room conversation. Messages and Contacts own person/group
messaging and contact identity; Chat adds no external-person inbox or contact store.

## Implemented journeys

A text channel exposes `share-resource(reference, text?)` through its published resource.
`reference` has the existing `tilo_reference::ResourceRef` shape: `kind`, `id`, `label`,
optional `span` (`start`, `end`, `unit`: `Chars` or `Bytes`). The action creates an
attributed message through `ChatAction::SendMessage`. Its address survives transport,
history and edits of the target; its label is presentation. No bytes, document snapshot,
capability grant or media stream are copied. Target access must still be checked by its
owning resource. Invalid addresses and reversed spans are refused on construction and
replay. Voice channels have no timeline and offer no resource-post action.

`read-message` includes `reference` (null for ordinary messages), so automation and
accessibility can inspect the address without parsing prose. The channel's paginated
`read-messages` remains a bounded text reader; use individual `read-message` for metadata.

Calls use the existing shell native-media service. Joining begins muted; unmuting
requires current write authority and the shell's fresh microphone consent. Leaving and
muting fence pending replies. `call-status` requests a refresh and returns
`{channel, reported, refreshRequested}`. `reported` is the last local service snapshot,
not a synchronous acknowledgement of the requested refresh. It exposes capture state,
selected input, issues and verified stream inventory. Call state never becomes durable
microphone consent. This increment changes no media transport and has not been tested
with two physical devices or microphones.

The device-only `open-slack-channel(workspace, channel)` and
`open-discord-channel(server, channel)` effects validate service ids and hand an HTTPS
link to the host URI opener on a worker. They use the existing optional `open-uri`
capability. The service's browser app owns sign-in and channel permissions. These are
browser adapters, not native clients, history importers or voice bridges. No tokens,
OAuth installation, network socket or service account are added to Chat.

## Live context

Chat now contributes opt-in channel, thread and scroll state to the SDK's existing
viewport presence. `share-live-context`, `stop-sharing-live-context` and
`live-context-status` are local, device-only effects. Sharing begins off and never
survives relaunch or becomes microphone consent. The conversation header shows sharing
or following status, with a shared Stop sharing button while publication is active. The shell's existing `presence.follow` and `presence.follow.stop`
actions choose/release the target, with its existing visible follow chip and roster.
Incoming context moves Chat only while the shell has selected a follow target and only
to existing, unarchived text channels and top-level threads in the loaded room. Stopping
publication emits an empty view contribution; followers stop adopting Chat navigation.
Existing collaborative draft caret presence remains independent of this optional
navigation publication. No ScreenCast or desktop pixel stream is used.

The shell's existing live-app selection remains responsible for cross-app navigation.
This increment adds Chat's semantic view contribution, not a new following transport or
per-person permission model. Two physical devices and the complete cross-app shell
journey still need manual validation before release.

## Remaining integration boundaries

The runtime includes existing-admission browser link resolution and navigation helpers
(details below), tested with action-frame fixtures. The production room action service
does not yet supply local-view metadata for other admitted collaborators. Native Chat chips still need host-provided source route metadata
and shared identity account-picker integration. `ResourceRef` alone conveys no access capability;
resource navigation links never grant admission.

## Compatibility research (2026-10-05)

Slack's [Calls API](https://docs.slack.dev/apis/web-api/using-the-calls-api/) represents
calls hosted by the integrating app; it does not expose Slack's call media. Its
[remote file API](https://docs.slack.dev/messaging/working-with-files/) can present a
browser URL to an external resource, but requires an installed authorized app and
scopes. Those integrations need the browser resolver and approved installation before
activation. [HTTPS deep links](https://docs.slack.dev/interactivity/deep-linking/) are
the implemented limited handoff.

Discord's [OAuth2 documentation](https://docs.discord.com/developers/topics/oauth2)
restricts user voice/DM scopes to approved partners and prohibits automation of standard
user accounts. Bots are separate identities, not transparent user clients. Its
[voice protocol](https://docs.discord.com/developers/topics/voice-connections) has its
own gateway, transport and DAVE encryption. No self-bot or native Discord voice claim is
made here; the implemented effect opens the service's browser channel.

## Interrupted following and authenticated lookup

The shell now releases room app following during reconciliation when its live
participant disappears, its named tab closes, established content focus is withdrawn,
its live session changes, or local semantic navigation selects another window, before
another paint/input pass.
A reconnected session does not inherit the released relationship. Fixture coverage
uses the real shell focus path and room follow dispatcher across two app subscribers,
with a separate-room subscriber proving isolation, and moves Chat → Terminal before
closing the target, withdrawing focus, local semantic focus, replacing its session or retiring the remote
session. This is a routed in-process fixture,
not a physical two-device or encrypted-network end-to-end result.

The browser's certified peer now offers `resolveResource(reference, provider)`.
`provider` must be the host-issued source record token; kind/id alone cannot distinguish
two records. Lookup uses the existing authenticated room action catalogue with its
permission filtering, exact kind/id matching and a 256-row budget. It rejects a
connection replacement or disconnect before accepting the answer. Labels and view
descriptors come from the live publisher. Lookup creates no invitation, persistent
grant, follow relationship or action invocation. Remote action frame fixtures cover
revocation, mismatched records, disconnection and bounded-search failure.

`openResource(target, reference, provider, action, {signal})` extends lookup into an
existing-record standalone browser mount. It waits for the admitted room's record,
passes its explicit existing id (including the canonical package-primary id), then
rechecks the host catalogue after hydration and the local app's current action catalogue, then invokes the chosen declared
resource view. It never invokes a document mutation or navigates the host's window.
Only explicit `ResourceViewSpec` navigation declarations qualify: read queries with
`OperationKind::View` alone do not. Chat declares workspace, text conversation and
message views through its existing navigation effects; opening a voice channel is
excluded because it would join a call. Cancellation, disconnect, changed connection,
missing views and refused invocations clean up the provisional mount. Record waits
are cancellable and reject when leaving the peer.

A resource view can declare a `ResourceViewRangeSpec`: a separate view action and
explicit integer start/end plus text unit parameters. The declaration survives both
the JSON catalogue and Cap'n Proto publication; the broker rechecks its binding.
`Chars` and `Bytes` retain the reference's unit. Unsupported, backwards, unsafe or
stale ranges fail instead of silently opening the whole resource. Chat message views
publish `reveal-message-range`; the current message validates Unicode boundaries,
then the existing read-only rich-text editor highlights that exact range. The highlight
is local and disappears when the source text changes. Other apps must declare their
own supported range view; this change does not claim they all do.

The certified peer's `resourceLink(page, reference, provider, action)` creates the
versioned browser link only after fresh authorized lookup. This is exercised with
frame fixtures; production host view discovery is an integration blocker below.
Its fragment carries the HTTPS host origin, room, host device key, exact source provider, kind/id, optional
span and declared view action. It carries no invitation, account key, bearer grant or
caller-supplied label. `resource.html` opens the standalone resource after an explicit
Open, using the recipient's existing browser identity profile and certificate. A
missing certificate is refused without pairing or renewal; current host/room/device
binding and permissions are checked before opening. Leave aborts pending navigation,
closes the app and releases the peer/device key. This is the runtime's shipped browser
entry point, not a newly deployed service.

Remaining integration: native Chat's legacy `ResourceRef` still does not carry the
source provider or host/room locator. Browser peers supplied with admitted source
view metadata can mint and open links in frame fixtures. The native Chat resource
chip's Copy/Open link path needs host-provided route metadata. The browser entry currently asks for the recipient's
existing public identity key; account-picker integration belongs to the shared identity
flow. New admission continues through the existing governed invitation flow, never
through this navigation address. Physical browser/native cross-device acceptance and
hosting distribution are unproven here. No access grant or deployment is added.

The existing browser-peer HTTPS fixture
`the_owner_admits_and_removes_a_different_person_over_https` also passes on the remote
builder, exercising actual owner-governed admission and removal with fixture identities.
Browser action-frame/resolver tests run as lightweight Node tests; no physical devices,
new persistent credentials or access grants are involved. The repository's mixed native /
browser end-to-end harness (`tilo-web-desktop/scripts/collab-e2e.mjs`) currently requires
macOS Accessibility permission and native macOS apps. It cannot establish the complete
Linux cross-app rendered journey on the selected cloud builder. The routed shell and
SDK fixtures should not be represented as that missing end-to-end acceptance.

## Representative range views and integration blockers (2026-10-06)

Chat messages and Notes blocks declare `ResourceViewRangeSpec`. Notes' existing block
`open` view now declares `reveal-block-range`; it accepts Unicode scalar (`Chars`)
or UTF-8 boundary (`Bytes`) offsets against the current block text. It validates
before navigation and checks the same text again after binding the shared editor.
A changed block discards the pending selection. Neither view rewrites document text.
These are positional offsets, not CRDT anchors: an in-bounds historical offset alone
cannot detect an earlier edit that shifted its intended meaning.

Notes keeps blocks lazily addressable through its registered resource kind. Exact
read-only discovery resolves that child without initializing an app or expanding the
ordinary document-operation catalogue. Word Processor's character navigation is not
yet a declared block resource view; apps without a range binding refuse ranged links.

The browser page uses an explicit account picker over the existing Settings-owned
`tilo.accounts` record. Only enabled `tilo.identity` entries with the selected public
root and exact HTTPS host appear. Its projection reads the canonical record key and
checks the document stamp and person-room scope; it neither opens a vault nor creates
custody. Account selection is checked again before connection and after hydration.
Leave closes the mount, peer and custody; a changed account cannot replace the old
profile during an outstanding request.

The continuation adds `CatalogueQuery::view_resource` to the existing authenticated
remote-action exchange. The relay checks current certified Read on the exact source
record; its worker replays bounded existing changes and surveys registered local
views without mounting Chat as a duplicate room record. The ordinary document door
retains its owner gate. Fresh role/capability checks precede returning any descriptors
or the host-issued HTTPS origin, room, device and provider locator.

The native broker uses its existing certified-channel read context and the shell's
existing remote-action transport to request that metadata, checking source Read and
current selected host/device before and after the request. A locator is navigation
metadata, never admission or an invitation.

The native Copy/Open chip and fully paired browser journey remain incomplete. The
relay's HTTPS API does not serve the browser bundle, and no deployed browser-entry
URL is configured in this repository. The host/browser deployment owner must supply
that destination; it cannot be inferred from the source label or API origin. Settings
also needs its normal identity sign-in flow to publish the public `tilo.identity`
account entry. The picker must not invent an account or certify a device on link open.

The governed HTTPS fixture currently reports an authenticated, encrypted, durable
source publication, but the host's duplicate peer waits for MLS keys. It uses the
same governor device identity with empty member state while the governor already
holds its own steward state. A proposed exact-source read adapter reuses those
existing keys and the shared signed-frame verifier, with certified Read before
key access and after bounded replay. Automatic approval review rejected that
security-sensitive production access change; it is pending explicit approval.
A mocked catalogue, a duplicate certificate, or a plaintext downgrade would not
prove the native-to-browser journey.

## Approved integration contract and authorization invariants

The following narrow adapters complete navigation; they do not change document
invocation authorization, enrollment, membership, or credential custody.

1. The relay authenticates user/device from the verified connection, never request
   JSON. Discovery requires an active certified device, current governed room
   admission, and current Read capability for the source app and requested resource.
   Owner, Editor, Commenter and Viewer can read only where that same capability
   engine allows it. Denied, unknown, removed, revoked, expired, detached and withdrawn
   devices receive no view metadata or locator. Agents retain their delegated scope;
   an ordinary person lookup cannot convert an agent into a person.
2. A request names one existing source provider, resource id and kind. The host
   resolves the current record and lazy resource through the publishing resource's
   existing door. It returns declared View descriptors only. It does not execute
   a view remotely, launch an app, create a record, expose document operations,
   or implicitly join a room. The existing document action catalogue stays intact.
3. Authorization is checked before reading and again before delivering metadata.
   The browser checks again after hydration. Revocation, session replacement or
   a changed record/source invalidates the operation, including cached local views.
   Permission checks use the canonical capability model and current policy time.
4. A locator contains only the configured HTTPS browser origin, current room, host
   certificate's device public key, exact record provider, resource kind/id, declared
   open action and optional range. It contains no account id, private state, label,
   invitation, token or certificate. Wildcard listener addresses are not public
   locators. A host without a routable configured browser origin refuses issuance.
5. The browser picker reads only a bounded public projection of Settings' existing
   shared accounts; it never enumerates vaults or invents a separate identity store.
   Selected account id and identity profile must still agree with a fresh projection
   before opening. Removed, disabled, changed or ambiguous accounts refuse. Account
   switching aborts pending navigation, closes the old mount/peer/custody, clears
   old account presentation and cannot reuse old admission or cached document state.
   Missing certification or membership still follows the existing governed flow.
6. Native Copy/Open and the painted resource chip use one public semantic action
   path. Copy mints only a navigation address; Open asks the existing URI host
   service. The external recipient opens the browser app. Leave is explicit,
   visible and cancels late results; this path requests neither live following,
   microphone permission nor video capture.

Affected owners/files, kept separate from the UI crate split:

- `crates/collab/actions/src/remote{,/catalogue}.rs`: bounded read-only request/reply
  vocabulary and public locator DTO, without modifying invocation semantics.
- `crates/sdk/tilo-resource/src/kind.rs`: existing-resource metadata lookup,
  including the publisher's lazy children, without executing effects or refactoring
  the SDK participant.
- `crates/agent/agent-participant/src/{apps,room_actions}.rs`: record-backed metadata
  resolution, capability filtering and two current authorization checks.
- `crates/hosting/tilo-relay/src/{agents,rooms,daemon,web/mod}.rs`: authenticated
  governed-room policy adapter and listener-owned locator origin. Existing admission
  and document-action authorization remain the authority.
- `crates/system/accounts/src/` and Settings' existing account projection: a public
  browser navigation projection of the same record, no Contacts schema/editor change.
- `crates/browser/tilo-browser-runtime/web/`: the account-picker adapter, lookup,
  native/browser link codec and cancellable page lifecycle.
- `crates/collab/tilo-action-broker/src/{services,collab_broker_capnp}.rs` and its
  `committed_document.rs`: reuse the original admitted channel for native queries.
- `crates/session/shell/src/shell/{broker_services,network/governor_actions}.rs`:
  current-owner Read authorization and the existing authenticated remote-action link.
- Chat's existing effects/resources/rendering: the future visible chip Copy/Open
  controls must use the same public action surface; these controls are incomplete.

The acceptance fixture must originate an actual Chat typed reference, obtain its
locator through authenticated production discovery, choose an existing projected
account, hydrate the same existing record, and invoke the browser's declared local
view. Companion fixtures refuse unauthorized roles/devices and changed sources,
revoke while discovery/hydration is pending, switch accounts mid-operation, and
leave before/after mounting. Fixtures alone use temporary governed identities;
no external grants or hosting deployment are authorized by this work.

### Pending host-reader adapter review

The failing fixture is
`an_existing_native_chat_reference_discovers_local_views_over_the_governed_https_host`.
Its sender has one durable operation, no failures, no outgoing work and no missing
keys. The service's duplicate governor peer has one sealed operation waiting for
keys. This is an MLS state ownership mismatch, not an absent caller grant.

The proposed implementation is confined to these owners:

- `rooms.rs`: authenticate the current caller and exact record Read again while
  holding the governed room lock, before accessing any key or retained payload.
  Filter the exact app/room/record, require encrypted signed frames, use the existing
  browser-peer `open_frame` and reassembly implementation, and replay at most 16 MiB.
  Refuse incomplete transfers and recheck certified Read before returning an internal
  snapshot and revision. No plaintext document is returned as a browser API response.
- `rooms/stewardship.rs`: expose existing content keys for an already-open exact
  channel to the host's reader; absence refuses. Never call `open_or_create`, create
  a channel, change membership, or issue a certificate from this read path.
- `browser-peer`: reuse its existing content-key cipher through a narrow exported
  adapter. Do not implement a second signature or encryption protocol.
- `agents.rs` and `RoomRecordsLink`: pass the already-authorized internal snapshot
  to exact view discovery instead of opening a second peer with empty MLS state.
  Default implementations refuse. The worker still surveys only inert declared
  views through the registered resource kind and checks fresh canonical capabilities.

Automatic approval review rejected applying this production access adapter. The
proposal is pending explicit approval; no part of the rejected command was executed.
Before it is published, the governed HTTPS fixture must pass, with additional
unauthorized/revoked-device and missing-source refusals. The ordinary document action
service and its invocation authority must remain unchanged.

### Read-only browser deployment discovery

The sibling Cloud repository owns browser hosting. Its
`deployment/aws/template.yaml` publishes the public console origin as the
CloudFormation `SiteUrl` output, derived from `SiteDistribution.DomainName`.
The configured runtime paths are `/qos-runtime/` and `/qos-session-runtime/`,
not an inferred path on the native relay API. `apps.tilo.so/artifacts/` in the
infrastructure operation notes is an application-package destination, not evidence
of a hosted resource viewer.

A concrete `SiteUrl` could not be retrieved with existing access: read-only
CloudFormation `describe-stacks` queries in the documented us-east-1/us-west-2
regions returned an expired default AWS session. No login, role switch, credential
change, or deployment was attempted. The Cloud deployment owner must retrieve the
existing output and selected runtime release; this does not require a user-invented
browser URL.

There is also an exact packaging gap. Cloud's
`scripts/prepare-qos-runtime.mjs` selects authored `.mjs` modules, package bindings,
and fonts. Its production `deployment/aws/scripts/deploy.py::runtime_assets`
allowlist similarly excludes HTML, and `runtime_delivery.py` publishes the selected
assets under a digest release with a stable `cloud.mjs` alias. Neither establishes
`resource.html`; the frontend contains no resource-viewer route. Tilo's native
browser build already copies `resource.html` into its output, but that does not
make Cloud publish it. The existing Cloud packaging fixture asserts that an HTML
page is excluded. A supported resource entry needs an explicitly inventoried,
versioned production-page route owned by Cloud, rather than admitting arbitrary
HTML from the build directory.

Cloud's response-header policy also limits connection destinations to the configured
packet relay and listed AWS/Cognito origins. Its hosting owner must verify that the
resource viewer's admitted relay origin is supported by that existing policy. This
investigation does not change CSP, CORS, trust, or permission configuration.
