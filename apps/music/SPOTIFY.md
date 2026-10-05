# Spotify in Music

Music keeps local files and YouTube Music, and adds a Spotify catalogue with
independent native playback and official Connect controls. Catalogue selection
is device-local and persists across restarts; a queued track keeps its provider
identity when the catalogue changes. The live app starts empty. The
`spotify-home`, `spotify-devices` and `spotify-error` captures use fixtures only.

## Authorization

Register a public Spotify client owned by Tilo or its distributor. Supply its
public ID through `TILO_OAUTH_SPOTIFY_CLIENT_ID` or the system account-custody
client table, and register `http://127.0.0.1:47654/oauth` exactly. Browser sign-in
uses the existing PKCE S256 implementation and state-validated loopback callback.
There is no client secret, password prompt, pasted bearer token or borrowed
Spotifast registration.

Connect Spotify authorizes catalogue, library and official player operations.
Authorize playback obtains a separate streaming grant after confirming Premium.
Both grants must identify the same account. Account custody seals them under
`music.spotify` and `music.spotify.streaming`; they never enter Automerge,
resources, the playback profile, fixtures or continuity payloads. Disconnect
forgets both device-local grants and stops the local Spotify transport. Revoke
provider access through Spotify account settings when needed.

A 401 refreshes the relevant grant once. A revoked grant requires sign-in;
429 Retry-After prevents immediate repeated requests. Redirects are disabled,
HTTP requests have bounded deadlines, response bodies are limited to 2 MiB,
and server-supplied pagination URLs are never followed. Playlist writes are not
automatically retried. Host operations time out rather than remain Accepted
forever, and warn that a timed-out write may already have happened.

## Listening

The optional `spotify-native` feature is enabled in Music's desktop defaults.
The `tilo-spotify` platform adapter uses librespot 0.8 with Tilo's own explicit
client ID, a separately authorized Premium account, 320-kbps selection and a
custom decoded-PCM GStreamer sink. It does not fabricate media URLs, substitute
YouTube matches, or call preview audio Spotify playback. Credentials and PCM
work stay on a worker; the UI does not join a network worker.

Play actions complete only after decoded PCM reaches an observed real sink.
Unavailable tracks, session rejection, fake sinks, output failures, missing
streaming authorization and account mismatches fail explicitly. Pause, seek,
volume, end-of-track and cancellation use the native transport. The last track
waits for its clocked output tail before advancing.

Sequential Spotify queues preload the next locally selected track and retain
one session/player/PCM timeline. Repeat-one and linear repeat-all use this path.
Queue changes replace the pending identity; a mismatched transition stops
instead of claiming the wrong track. Mixed-source and shuffled queues use the
ordinary source transition. Live gapless timing and decoder transitions still
require Premium-account acceptance testing. Native playback is not advertised
as a Spotify Connect receiver, and Spotify-native continuity readiness remains
unpublished until that contract has been verified.

Protected Spotify audio cannot be downloaded through Music's offline cache.
Spotify lossless DRM is unsupported. The page build retains its media-element
backend and does not enable native Spotify dependencies.

## Catalogue, library and devices

Home shows Liked Songs, the first 20 saved albums and the first 20 playlists.
Mixed and per-type search return up to ten results per requested type. Albums,
playlists and Liked Songs use bounded paging, with an explicit 2,000-track
limit; an incomplete or oversized collection is refused rather than presented
as complete. Artist pages currently show releases rather than invent top tracks.

Track menus offer explicit Spotify save/remove, separate from Tilo's shared
likes. Save/remove actions read back library membership. Playlist rename reads
back its name; add/remove require a provider snapshot revision. Remove deletes
all occurrences of the track. These edits are provider effects, never shared
Tilo document mutations. Playlist editing is also available through the resource
action surface; its full graphical editing journey needs further design work.

Spotify devices offers official discovery, output selection, play and pause.
Premium is checked before a control command, and restricted/unavailable devices
are refused. A 204 response is only an acknowledgement: Music reads the player
state and confirms the device, track and playing state before reporting success.
Confirmed remote play pauses Music's local transport. Spotify owns the remote
queue; Music does not claim local reorder/remove controls edit that queue.
Device resources expose the complete discovered list; the Home control group
shows up to twelve devices.

## UI and resource surface

Shared buttons provide source, authorization and device controls; source and
error text wraps, controls wrap to the viewport, and their geometry scales with
system text size. Home playlist tiles now paint artwork and open their actual
provider collection. The large-text Home renderer includes remote collections,
wraps empty-state headings, and scrolls the source controls with the content.
Search hints describe the selected catalogue instead of assuming YouTube.

Search/library entities, provider save/remove, playlist edits, Connect and
native playback are exported through resources/actions. Spotify's track-menu
save/remove emits the same host-admitted action as scripts and accessibility.
The sandbox declares optional browser/secrets capabilities; no access policy
has been weakened.

Spotify playlist tiles and detail headers expose the shared context menu for
save, remove from library and rename. The existing shared rename dialog starts
with the provider name. The track playlist picker includes known Spotify
playlists, and provider playlist tracks offer removal of all matching entries.
These controls emit host-admitted provider effects rather than writing remote
changes into the local CRDT. Ownership/permission failures remain visible.

## Acceptance boundaries

This remains a parked draft, with no merge or deployment authorized. A local
implementation and mock/PCM tests cannot certify Spotify service eligibility.
Real browser authorization, the live secret store, revocation, streaming,
Connect confirmation and uninterrupted gapless playback need a Tilo-owned
registration and an eligible, consenting Premium test account. Spotify account
allowlists, quota mode and streaming eligibility apply independently of this UI.
No credentials or grants were created during implementation.

The competitive UX goal is not yet certified: further acceptance covers playlist
creation and ownership guidance, library paging beyond the current bounds, followed artists,
remote queue presentation, all search/queue/playback states at large text,
keyboard/screen-reader journeys and responsiveness on older hardware.

## Reference sources

The architecture follows Spotifast's separation of Web API authorization and
native streaming sessions, implemented independently. Spotifast and librespot
are MIT-licensed and compatible with this AGPL workspace. No Spotifast code,
artwork, icon, assets, client identity or authentication workaround was copied.

- [Spotifast source and licence](https://github.com/crmne/spotifast)
- [Spotifast connection architecture](https://spotifast.rocks/how-it-connects/)
- [Spotify PKCE](https://developer.spotify.com/documentation/web-api/tutorials/code-pkce-flow)
- [Spotify quota modes](https://developer.spotify.com/documentation/web-api/concepts/quota-modes)
- [Spotify playlist items](https://developer.spotify.com/documentation/web-api/reference/get-playlists-items)
- [librespot](https://github.com/librespot-org/librespot)
