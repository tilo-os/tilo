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

Notes keeps blocks lazily addressable through its native action door rather than
publishing every block into the bounded room catalogue. The browser reference resolver
currently reads that catalogue; Notes block browser routing therefore remains
unsupported until authorized exact-resource discovery is available. A native range
contract does not establish a complete native-to-browser sharing journey. Word
Processor's existing character navigation is not yet a declared block resource view;
its search/caret operations must not be advertised as a browser range contract.
Other apps without an explicit range binding refuse ranged browser navigation.

The runtime page now shares a tested navigation lifecycle. Leave during connection
or hydration disposes late results, does not close a replacement session, and still
disposes the peer if app cleanup fails. Fixtures cover the page's production lifecycle
and resolver refusal, but do not provide native host-route or account-picker APIs.

Three existing ownership boundaries prevent finishing the essential sharing path:

- The browser room action service (`agent-participant/src/room_actions.rs`) builds
  its catalogue from `Replica::survey`, which intentionally exposes document
  operations only. It accepts the host user's devices, not arbitrary admitted
  collaborators (`RoomActions::authorized`). The browser resolver's local-view rows
  are exercised by frame fixtures, not currently supplied by that production service.
  Exact `CatalogueQuery::target` filters an existing snapshot; it does not synthesize
  lazy resources or view metadata. A read-only, role-filtered resource/view discovery
  endpoint is needed. Do not expand document invocation privileges to enable it.
- The host's HTTPS origin is owned by `tilo-relay::web::WebHandle`; native app/broker
  resource metadata currently has no host-issued browser locator carrying origin,
  room, host device and source provider. A label, local path or caller-supplied URL
  cannot supply this authority binding. Chat's native Copy/Open chip is incomplete.
- Settings owns `accounts::person::PersonAccounts` (`tilo.accounts`). Contacts owns
  recipient associations, which explicitly convey neither identity proof nor browser
  admission. The browser entry has no read-only account enumeration/selection bridge
  to that shared record; root-identity state is in-memory and device custody is keyed
  by an already selected identity profile. The entry still asks for a public key.
  An owner-provided account projection and identity-profile mapping are needed before
  replacing this with a picker. No second account registry is introduced.

The held PR must remain held until those contracts are integrated and a fixture proves
native Copy/Open through the browser entry, including revocation and Leave. Existing
browser and HTTPS admission fixtures are supporting checks, not that acceptance test.
