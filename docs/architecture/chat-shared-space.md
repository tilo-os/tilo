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
