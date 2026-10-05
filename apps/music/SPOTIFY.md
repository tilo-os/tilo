# Spotify in Music

Music keeps local files and YouTube Music, and adds a Spotify catalogue with
independent native playback and official Connect controls. Catalogue selection
is device-local and persists across restarts; a queued track keeps its provider
identity when the catalogue changes. The live app starts empty. The
`spotify-home`, `spotify-devices`, `spotify-error`, `spotify-queue` and
`spotify-create` captures use fixtures only.

## Authorization

Register a public Spotify client owned by Tilo or its distributor. Supply its
public ID through `TILO_OAUTH_SPOTIFY_CLIENT_ID` or the system account-custody
client table, and register `http://127.0.0.1:47654/oauth` exactly. Browser sign-in
uses the existing PKCE S256 implementation and state-validated loopback callback.
There is no client secret, password prompt, pasted bearer token or borrowed
Spotifast registration.

Connect Spotify authorizes catalogue, library and official player operations.
Authorize playback obtains a separate streaming grant after confirming Premium.
Both grants must identify the same account; stable `account_id` values, when
returned, must also match. The native session additionally matches its canonical
username. A missing/deprecated subscription field is reported as unknown; native
playback stays unavailable rather than assuming Premium or mislabelling Free. Account custody seals them under
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

Home starts with Liked Songs and up to 50 saved albums and 50 playlists.
Load more Spotify collections advances by 50, up to 2,000 collections per type.
Only a successfully decoded page advances the cursor; overlapping IDs merge
once, failed/stale pages preserve the current library, and only one page request
can be active. Device/remote-queue refreshes preserve loaded library pages.
Offscreen Home tiles produce neither paint nor pointer targets; their canonical
resources remain reachable.
Mixed and per-type search return up to ten results per requested type. Albums,
playlists and Liked Songs use bounded paging, with an explicit 2,000-track
limit; an incomplete or oversized collection is refused rather than presented
as complete. Artist pages currently show releases rather than invent top tracks.

Track menus offer explicit Spotify save/remove, separate from Tilo's shared
likes. Save/remove actions read back library membership. Playlist rename reads
back its name; add/remove require a provider snapshot revision. Remove deletes
all occurrences of the track. These edits are provider effects, never shared
Tilo document mutations. Playlist editing is also available through the resource
action surface. The shared name dialog can create an empty private Spotify
playlist; creation requires a valid provider URI and private/name read-back.
A failed confirmation is uncertain and must not be automatically retried.
Rename checks ownership; song edits require ownership or collaboration before
sending the write. Saving somebody else’s playlist never grants editing rights.

Spotify devices offers official discovery, output selection, play and pause.
Premium is checked before a control command, and restricted/unavailable devices
are refused. A 204 response is only an acknowledgement: Music reads the player
state and confirms the device, track and playing state before reporting success.
Confirmed remote play pauses Music's local transport. Spotify owns the remote
queue; Music does not claim local reorder/remove controls edit that queue.
Device resources expose the complete discovered list; the Home control group
shows up to twelve devices. Refresh Spotify queue opens a separate, read-only
provider snapshot (up to 50 upcoming items, with truncation explicitly labelled).
Tracks and episodes show their metadata; snapshot age and errors remain visible.
The shared refresh button is in Queue’s host focus/accessibility tree, and
read-only remote item resources are exported. Refresh never replaces or edits
this device’s local queue.

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

The competitive UX goal is not yet certified: further acceptance covers
paging beyond the current bounds, followed artists,
all search/queue/playback states at large text,
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

## Dependency repair and verification

CI at `bc10f53dad` exposed librespot's old dependency chain: banned quick-xml
0.38.4, Rustls/webpki certificate-validation advisories, unlicensed webpki
metadata and OpenSSL path probing through native-certs 0.7.3. The old native-certs
version also made CI's package selector ambiguous.

The explicitly approved repair vendors checksum-verified MIT librespot-core
0.8.0 and hyper-proxy2 0.1.0. Core uses quick-xml 0.41, preserving its prior XML
content normalization. Proxy dependencies reuse the already locked modern
Rustls/ring stack and Tilo's unchanged fixed-path native-root loader. The unused
old webpki edge and obsolete TLS/probing packages are removed. Upstream TLS
feature defaults are retained; Music still disables defaults and explicitly
selects Rustls with native roots. No native-TLS switch, insecure verifier,
certificate-policy exception or alternative trust loader is introduced.

Non-Premium product updates now invalidate the session and return a recoverable
permission error instead of upstream's process-wide `exit(1)`. Both attribute
setters also fail closed, rejected sessions cannot be revived by later
attributes, and Music stops output with an error when a session becomes invalid.
Malformed product XML cannot replace previously accepted attributes.

Four focused vendor tests passed: actual product-info dispatch, both attribute
setters, malformed XML, and real local CONNECT/TLS handshakes that reject an
unknown issuer and a wrong hostname but accept a valid certificate. Testing
used temporary vendor workspace membership, then restored the production
manifest and audited lock before the 107-test Music/native regression run.
Cargo-deny passed advisories, bans, licences and sources without exceptions.
The lock introduces no package version and moves no unrelated dependency;
ten obsolete dependency packages are removed. Vendor `PROVENANCE.md` files
record archives, checksums, licences and the exact modifications.

The earlier broad proposal to switch TLS providers was rejected by automatic
approval review and never applied. The user subsequently authorized only this
narrower Rustls-preserving repair. The PR remains draft and on hold.

Chromium's cancellation failure was reproduced on clean base `aad1bd2997`.
A later owner-commit CI timeout passed when tested alone on both base and
`bc10f53dad`; that timeout is unexplained, not classified as a base failure.
Neither test was ignored or weakened. New-head CI remains required.

## Live acceptance setup (not started)

Use a Tilo/distributor-owned public app registration and configure the exact
loopback redirect above. A consenting eligible Premium tester must be authorized
for that app; the app owner needs active Premium in Development Mode. New-user
allowlist limits and grandfathered registration differences apply. Verify the
selected registration's endpoint availability and whether it returns subscription
eligibility; absent product data currently blocks native playback. Start browser
PKCE only with explicit approval, authorize the catalogue and streaming grants
for the same account, and use the system secret store. Never paste tokens,
passwords or client secrets into chat. The client ID is public configuration.

Then verify cancel/denial, mismatch, expiry/revocation, sealed-store failure,
actual decoded audio/pause/seek/volume/track changes, gapless waveform timing,
output failure and repeated session teardown. For Connect, provide a real active
Spotify device and verify read-back on transfer/play/pause. Exercise playlist
creation/save/rename/song edits against owner, collaborator and non-owner cases.
No browser sign-in, account setup, live provider write or actual Spotify stream
was performed in this task.

- [Current Development Mode migration requirements](https://developer.spotify.com/documentation/web-api/tutorials/february-2026-migration-guide)
- [Current profile fields](https://developer.spotify.com/documentation/web-api/reference/get-current-users-profile)
- [Spotifast TLS dependency strategy](https://github.com/crmne/spotifast/blob/main/Cargo.toml)
- [Upstream non-Premium session handler](https://github.com/librespot-org/librespot/blob/v0.8.0/core/src/session.rs)
