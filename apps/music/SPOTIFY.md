# Spotify integration status

This draft adds a Spotify Web API catalogue alongside YouTube Music and local
files. Home offers browser sign-in, saved albums and playlists, and source
selection; search uses the existing multi-type result UI. Album and playlist
tracks and artist releases are paginated, with an explicit 2,000-item bound.
Home shelves show the first 20 saved albums and playlists; mixed and per-type
search return the first 10 results of each requested type.

## Configuration and custody

Register a public Spotify client owned by Tilo or the distributor. Supply its
public ID with `TILO_OAUTH_SPOTIFY_CLIENT_ID` or the shared account custody client
table. Register `http://127.0.0.1:47654/oauth` exactly. This uses the existing
PKCE S256 client and state-validated loopback callback; there is no client
secret, password prompt, pasted bearer token or borrowed Spotifast identity.

Grants are sealed by system account custody under the device-local
`music.spotify` account. They never enter Automerge, a playlist, a playback
profile, a screenshot fixture or a continuity payload. Disconnect forgets the
local grant; provider-side revocation is available in Spotify account settings.
An expired access token is refreshed once after a 401. A revoked refresh grant
requires another sign-in; a 429 respects Retry-After without an automatic retry
loop. Source selection is device-local, and existing track routing remains
independent of the selected catalogue.

The sandbox manifest declares browser launch and secrets as optional
capabilities. This change does not weaken filesystem or network sandbox policy.

## Playback and remaining acceptance work

**Spotify audio is not implemented by this draft.** Play and offline download
attempts return an explicit error. Metadata is not converted to YouTube search
results, preview URLs or fabricated decoder URLs. A future playback engine
must use a distinct source transport, confirm decoded audio before completing
a play action, and publish continuity readiness only after real output.

Spotifast's strategy separates Web API authorization from librespot's native
streaming session. Its MIT licence is compatible with this AGPL workspace, as
is librespot's MIT licence. This implementation is independent: no Spotifast
code, artwork, icon, client registration or authentication workaround is copied.
Native librespot playback requires Spotify Premium and remains unofficial.
Spotify lossless DRM is unsupported by librespot. A Free account must never be
presented with fabricated successful playback; controlling other devices also
requires Premium. Spotify's registration, account eligibility, development
allowlist and quota rules apply independently of the local UI.

Before this draft is ready to land:

- Implement and test the separate native streaming session and audio output,
  Premium gating, cancellation, gapless queue transitions and decoder failures.
- Add Spotify Connect devices, real transfer confirmation and remote player
  state. Remote Spotify queues cannot inherit local reorder/remove semantics.
- Complete library editing, Liked Songs and followed-artist browsing, incremental
  pagination and source-selection persistence.
- Validate real browser sign-in, the system secret store and account revocation
  with a Tilo-owned client and an eligible consenting test account.
- Review search, library, queue, auth, error and playback views in light, dark,
  high contrast, narrow layouts and large text; measure responsiveness and
  effects on older hardware. The new shared source controls wrap at narrow
  widths, but this is not yet the requested full competitive UI redesign.

## Reference sources

- [Spotifast source and licence](https://github.com/crmne/spotifast)
- [Spotifast connection architecture](https://spotifast.rocks/how-it-connects/)
- [Spotify PKCE](https://developer.spotify.com/documentation/web-api/tutorials/code-pkce-flow)
- [Spotify quota modes](https://developer.spotify.com/documentation/web-api/concepts/quota-modes)
- [Spotify playlist items](https://developer.spotify.com/documentation/web-api/reference/get-playlists-items)
- [librespot](https://github.com/librespot-org/librespot)
