# Chat shared-space increment

Chat is the persistent Tilo room conversation. Messages and Contacts own person/group
messaging and contact identity; Chat adds no external-person inbox or contact store.
Bridges to outside services are bots installed into the room, not part of Chat (see
"Bridges" below).

## Implemented journeys

A text channel exposes `share-resource(reference, text?, sourceProvider?)` through its published resource.
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
OAuth installation, network socket or service account are added to Chat. A two-way
mirror of a workspace is a bridge bot with its own connector (see "Bridges"); it holds
the token and the socket, and Chat still holds neither.

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

## Bridges

A bridge mirrors a Chat workspace to an outside service. It is built from the bot
framework ([bots.md](bots.md)): a connector app holds the service's credentials and
socket, and a bot program maps Chat's changes to the connector's verbs and back. The
Discord bridge ([discord-bridge.md](discord-bridge.md)) is the first. Chat stays
service-agnostic: it learns nothing about Discord, and every change below applies to
every workspace, bridged or not.

- **Appearance.** A bot allowed `Personas` may post under another name and avatar.
  `post`, `share-resource` and `react` read `ActionCx::appearance()` and store it:
  `Message.appearance: Json<Option<Appearance>>` and `Reacted.persona: Option<String>`
  (new), so two Discord users' identical reactions count as two. The author is still
  the bot. Every message author is drawn through the shared `ui::author` helper, so a
  mirrored message shows the outside name and avatar with the `BOT` label and "via
  Discord Bridge (bot, added by …)" in its details.
- **Avatars for people.** `Person.avatar: Option<BlobRef>` (new) is set through
  `introduce(name, initials, color, avatar)`. The image is a content-store blob of at
  most 1 MiB. People without one keep their initials.
- **Author checks.** `edit-message` and `delete-message` refuse anyone but the
  message's author (`cx.author()` against `Message.author`). Until now neither checked,
  so any Editor could rewrite anyone's message. `delete_message` gains the `cx`
  argument it needs for this.
- **Moderation.** Three verbs carry `#[action(moderation)]`, which places them under
  the room's `moderation` node ([permissions.md](permissions.md) §3): `remove-message`
  (deletes any message, recording who removed it and an optional reason in history),
  `lock-channel` and `unlock-channel`. `Channel.locked: bool` (new) makes `post` and
  `share-resource` refuse in that channel for everyone, Moderators included, until it
  is unlocked. Kicking and banning a member are room governance (`remove-member`,
  `ban`), not Chat verbs, and Chat lists them on a member's card for those who may use
  them.
- **Incoming messages.** `post` and `share-resource` are marked
  `#[action(incoming_message)]`, so a person's `IncomingMessage` Shortcut fires on
  other people's chat messages ([bots.md](bots.md) §4).

## Remaining integration boundaries

The production room action service supplies declared local-view metadata through
fresh certified Read checks and bounded replay of existing source state. Native Chat
Copy/Open effects use that host-provided route, and the browser entry reads Settings'
existing identity choices. The paired browser journey is fixture-based; an operator
must configure and deploy the browser entry before external links work against a live
host. `ResourceRef` alone conveys no access capability, and resource navigation links
never grant admission. Physical cross-device and microphone acceptance remains unrun.

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

Browser links to a resource use the one link grammar of
[web-routing.md](web-routing.md): `entry["app/"]#[/@host]/w/[slug~]room/app/record/resource`
with an optional `view` and `chars`/`bytes` span, formatted and parsed only by
`tilo_reference::link`. The fragment carries no account key, bearer grant or
caller-supplied label; an invitation rides in it only when the sharer chose to
include one, and the entry page strips it from the address before any module
runs. The desktop entry (`index.html`) and the full-tab app entry
(`app/index.html`) admit the visitor with their existing browser identity, then
open the target through the declared view action, so current room membership
and permissions are checked by the action door, not by the link. The separate
`resource.html` page and its versioned fragment are gone; the sections below
that mention them record the evidence gathered with that earlier page.

Native messages now keep the exact source provider beside `ResourceRef` and expose
public Copy/Open effects backed by fresh host-provided route metadata. Older
references without provenance remain readable and refuse browser navigation. The
browser entry projects existing accounts from Settings' personal record. Its
explicit public metadata publication action is available after sign-in; automatic
publication from every sign-in entry is still unwired. New admission continues
through the existing governed invitation flow, never through a navigation address.
Physical cross-device acceptance and a deployed browser entry remain unverified.
No access grant or deployment is added.

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

The isolated mounted journey now proves native Copy/Open and the browser account
picker against a configured temporary entry. A production host still needs the
operator-configured browser entry; the relay API does not serve that bundle.
Settings' normal sign-in flow still needs to publish its public `tilo.identity`
account entry. Link opening neither invents an account nor certifies a device.

The governed HTTPS fixture now passes using an exact-source reader of the
host's already-owned steward keys. It no longer opens a duplicate governor peer
with empty MLS state to discover views. Current certified Read is checked under
the governed room lock before key or ciphertext access, after bounded replay, and
again in the metadata worker before descriptors or a locator can be delivered.
That discovery fixture publishes a native-format Chat resource through HTTPS.
The separate mounted acceptance example now starts with the production native
peer publisher and opens its replicated Chat message in Chromium. Neither fixture
drives a physical compositor session.

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

### Existing-key host reader

The fixture
`an_existing_native_chat_reference_discovers_local_views_over_the_governed_https_host`
verifies encrypted durable publication, exact local View metadata and the
listener-issued locator. Wrong resource ID/kind returns no metadata or locator;
the same primary-provider reference in a different governed room is refused.
A production-worker fixture proves fresh Read succeeds and revocation between
snapshot reading and delivery refuses before locator issuance.

The implementation is confined to these owners:

- `rooms/views.rs`: authenticate the current caller and exact record Read again while
  holding the governed room lock, before accessing any key or retained payload.
  Filter the exact app/room/record, require encrypted signed frames, use the existing
  browser-peer `open_frame` and reassembly implementation, and replay at most 16 MiB.
  Storage reads check their byte budget before allocation and cap scanned entries
  at 8192. The existing committed basis and active retained selectors are read;
  no generation, retention, proof or collection state is changed. A selected basis
  spanning channels is conservatively charged to the same 16 MiB read budget.
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

The first implementation attempt was rejected by automatic approval review.
The user subsequently explicitly authorized the corrected design with authorization
under the room lock before key access. That one authorized retry executed; no
new grants, keys, memberships, device certificates or deployment were introduced.
The ordinary document action service and invocation authority are unchanged.

The real governed-room test checks uncertified/wrong-root/local/agent/app callers
are refused before existing-key access, wrong workspace is refused, and a reader
polled while waiting for the room lock observes a signed device revocation
committed before that lock is released. Bounded storage reads are tested against
existing retained checkpoints/tails and must leave history unchanged.

### Read-only browser deployment discovery

**Superseded (2026-10-14).** `tilo.coffee` is no longer Cloud's CloudFront
distribution: it serves the web entry image built from this repository
(`.forgejo/workflows/web-entry.yml`, `crates/browser/tilo-browser-runtime/container/`).
Its `/version.json` names the content version and source revision; the entry
pages import one `v/<version>/` directory whose files match that version's
`build.json` hashes, and everything under `v/` is immutable. `/qos-runtime/`
answers 404 there. The resource entry is the link grammar opened through
`index.html` or `app/index.html` ([web-routing.md](web-routing.md)), so
`resource.html` is in no release, and Cloud's `/qos-runtime/resource.html`
packaging is never fed: current bundles do not contain the page, and Cloud
publishes its resource entry only when one is present. The rest of this
section records the earlier Cloud-hosted layout.

The sibling Cloud repository owned browser hosting. Its
`deployment/aws/template.yaml` publishes the public console origin as the
CloudFormation `SiteUrl` output, derived from `SiteDistribution.DomainName`.
The configured runtime paths are `/qos-runtime/` and `/qos-session-runtime/`,
not an inferred path on the native relay API. `apps.tilo.so/artifacts/` in the
infrastructure operation notes is an application-package destination, not evidence
of a hosted resource viewer.

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

### Native browser navigation and identity publication

New `share-resource` messages can include `sourceProvider`, the existing typed
`record:<app>:<record>` provider. The message keeps that provenance beside its
resource address; it does not carry a host URL, bytes or access grant. Older
messages remain readable but cannot mint a browser link without provenance.
`read-message` exports the provider. A message carrying a resource shows
"Copy link" and "Open resource in browser" controls in shared Buttons. Both
invoke `share-link` on the referenced resource through the action door, so the
resource's own app (which knows its views) asks its room's host for a fresh
locator and renders the link grammar under the host's web entry; Chat builds no
link itself. Agents and automations call that same `share-link` directly. Open
hands the link to the browser, since Chat cannot show another app's view in
place. One request runs at a time with a 30-second limit, and its outcome is
checked against the message still carrying the same reference and provider. A
host without a web entry refuses with its reason, which the control shows.

Settings' device-local `publish-browser-identity` effect takes a canonical HTTPS
host origin and reads the process's bound public identity. It invokes the typed
`publish-browser-identity` action on Settings' existing personal accounts record;
no fallback registry, secret vault operation, certificate or membership is
created. Republishing preserves an existing account's disabled choice and name.
The account appears in Settings with shared visibility/removal controls and public
row actions. These affect picker metadata only; they do not revoke sign-in or
resource permissions. The Accounts area reads the same owned personal record,
including external-account forms, rather than creating a second registry.
This is an explicit public-metadata publication action after sign-in. Automatic
publication from every sign-in entry has not been wired; callers must use this
Settings action before the corresponding browser picker entry is available.

Cloud's separate change inventories `resource.html` and `resource.css`, with a
stable external-module redirect to one immutable runtime release. It explicitly
accepts the native Tilo desktop bindings, canonical fonts, app inventory and
attested data payloads, while preserving current native runtime aliases and
producer provenance/cache policies. The source
build now truthfully records `builtFromSource` in its existing hash manifest.
Packaging fixtures preserve the resource fragment, licenses and existing CSP.
Native navigation correlations fit JSON's exact integer range so browser peers
can return the same catalogue request identifier. The observed public Copy
result passes the browser resolver's exact hydration, range view, Leave and
post-hydration revocation fixture. No Cloud deployment, actual compiled desktop
bundle validation, physical microphone test or deployed native-to-browser journey
has run in this continuation.

## Mounted acceptance and rollout audit (2026-10-06)

Run the compiled desktop against the isolated governed fixture:

```sh
$CARGO run --locked -p tilo-agent-participant --example mounted-resource -- target/tilo-app/web/calculator/bundle
```

The example uses a native Chat replica and production native publisher, an owner
issued Viewer invitation, the real HTTPS relay, existing Settings account records
and fixture certificates, and Chromium on a disposable profile. Loopback TLS
credentials come from the existing Rust-native `tilo-crypto` fixture generator
and reach Node only over the temporary stdin pipe. It mounts the
compiled Chat at a Unicode character range, discovers and invokes its public
consent/Stop sharing actions, verifies no video capture, and exercises native
Copy into the actual browser clipboard and Open to the exact resource destination.
The production resource page refuses an unadmitted account without enrollment,
opens the selected admitted account, releases the mount and custody on Leave,
reopens the picker after navigation, and refuses reopening after owner revocation.
Its screenshot and stage report are written under `target/mounted-resource/`.
The existing app SDK web lane runs this journey; absence of the compiled bundle
or browser fails rather than skips. All identities and clipboard permission
exist only inside this fixture. No live account, desktop or microphone is used.

The mounted run exposed two real gaps: the CLI omitted `resource.html` and
`resource.css` from its browser release closure, and standalone Chat mounting
rejected the canonical package-primary existing record. Both are corrected.
Native catalogue requests now use the page's existing certified actions peer,
with late replies fenced by mount lifetime and connection identity. Catalogue
and media polling share the retained component request collector: neither can
steal the other's queued command, and closed owners lose their queued commands.
Native tests exercise both poll orders and owner retirement. Native Copy
reports the browser's actual synchronous clipboard result; no clipboard fallback
pretends a denied write succeeded.

The original isolated native voice-session CI failure closed before the expected
call-state event. Three bounded exact repeats passed; no race was demonstrated.
The assertion is preserved and now names its expected stage on closure. No
blind reconnect or test retry was added.

### Compiled standalone browser acceptance (2026-10-07)

The supported `tilo-app build apps/chat --platform web` path now recognizes
`ui::dev_reload_entry!` as exporting the SDK's existing `definition()`. The CLI
previously rejected Chat before generating its browser registry. No SDK macro or
Chat definition is duplicated. A focused CLI regression covers that production
source probe, and the actual Chat WASM bundle builds successfully.

Run `node scripts/headless-examples/chat-browser-journey.mjs <chat-bundle>`
against that generated bundle with the repository browser helper's `CHROME`
configured for an owned Chromium process. The journey serves only loopback and
uses a throwaway browser profile. It mounts the compiled Chat, requires a real
presented frame, creates two conversations through published actions, posts one
message and one typed live-resource address, reads them through the public
resource door, verifies conversation isolation and exact provider provenance,
flushes and reloads the same room, then compares the complete restored results.
The local compiled-browser journey passed with Chromium's sandbox enabled and
an owned descendant supervisor. It does not certify peer authentication, native
clipboard navigation, resource hydration, public hosting or browser voice.

### Governed compiled browser recovery

The existing native `mounted-resource` fixture now also qualifies the actual
Chat browser bundle over its temporary certified HTTPS host. The default Viewer
journey passed resource opening, browser Copy/Open, explicit account choice,
Leave and owner revocation. Its optional `--disconnect-retry` journey uses an
ordinary temporary Editor invitation, observes the real browser carrier lose
authentication, queues one original browser post while a native post becomes
durable, invokes the existing retry control, then verifies both original posts
exactly once in the browser and native replica. It requires a newly presented
frame and keeps the same certified device. The SDK web lane preserves its
Viewer journey and adds the actual Chat build/recovery journey with separate
receipts. See `docs/native-chat-browser-acceptance.md` for scope and reproduction.
These loopback fixture results do not establish access to a public room, browser
voice, a full profile restart with queued edits or cross-app Follow.

## Compiled browser call acceptance (2026-10-07)

The `mounted-resource --voice` journey now exercises two certified profiles
running the actual compiled Chat and Wasm peer, including production audio and
signalling. The governor prepares the registered native primary and authors its
existing graph through the host-local `Rooms::author_application` API. The API
checks exact current custody and record access, refuses missing or sealed
records before the callback, and publishes admitted incremental changes. It is
not an incoming member publication or a new network authoring endpoint. Native
resource replay and voice binding keep their existing admission predicates.

The journey passed muted join without capture, explicit Unmute through public
actions and the rendered semantic control, bidirectional decoded synthetic
audio, independent mute, Leave cleanup, rejoin with fresh consent, and targeted
owner revocation. The SDK web lane runs it with unprivileged sandboxed browsers
and retains screenshots and quantitative lifecycle observations. A regression
also proves that an authored record reaches the retained resource view when a
real signed governance tail crosses its publication limit.

This qualifies the governed loopback compiled-browser call. It does not
establish physical microphone support, arbitrary remote encrypted Chat routes,
cross-app Follow, or access to a public room. See
`docs/native-chat-browser-acceptance.md` for reproduction and the separate
Viewer and disconnect/retry journeys.
