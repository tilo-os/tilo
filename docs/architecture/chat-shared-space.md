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

External browser links to Tilo resources also require a deployed, authenticated browser
resource resolver. `ResourceRef` alone contains neither a public URL nor an access
capability; this increment does not mint public links.

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
rechecks the local app's current action catalogue and invokes the chosen declared
resource view. It never invokes a document mutation or navigates the host's window.
Only explicit `ResourceViewSpec` navigation declarations qualify: read queries with
`OperationKind::View` alone do not. Chat declares workspace, text conversation and
message views through its existing navigation effects; opening a voice channel is
excluded because it would join a call. Cancellation, disconnect, changed connection,
missing views and refused invocations clean up the provisional mount. Record waits
are cancellable and reject when leaving the peer. Ranges are refused until a
range-aware view exists, rather than silently opening the wrong position.

Remaining precise blockers to clickable external resource links: Chat's legacy
`ResourceRef` does not carry the source record provider or an authenticated host/room
locator; there is no canonical deployed browser link envelope/bootstrap that resolves
those and calls this API. Joining already uses `tilo-browser-peer::invite` with
root/device certificates and governed room admission. The new API reuses that authority
and adds no persistent grant. Joined desktop topology has a separate existing-record
restriction; this path uses the already-supported standalone browser mount.

The existing browser-peer HTTPS fixture
`the_owner_admits_and_removes_a_different_person_over_https` also passes on the remote
builder, exercising actual owner-governed admission and removal with fixture identities.
Browser action-frame/resolver tests run as lightweight Node tests; no physical devices,
new persistent credentials or access grants are involved. The repository's mixed native /
browser end-to-end harness (`tilo-web-desktop/scripts/collab-e2e.mjs`) currently requires
macOS Accessibility permission and native macOS apps. It cannot establish the complete
Linux cross-app rendered journey on the selected cloud builder. The routed shell and
SDK fixtures should not be represented as that missing end-to-end acceptance.
