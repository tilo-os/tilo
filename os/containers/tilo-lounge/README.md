# Tilo Lounge

One always-on public Tilo workspace, hosted by one process: `tilo-relay`'s
persistent public workspace, the deployment routes beside it, and the Discord
bridge whose bot programs it runs.

```
GET /mode                 {"relay":"up","revision":"<commit>"}   the readiness probe
GET /avatars/<name>       an avatar a bridged author published (immutable)
GET /discord/callback     Discord's OAuth redirect: links the account, then
                          sends the browser back to the web entry with its
                          one-time link secret
anything else             404; anything but GET     405
```

The workspace's own pages are the Tilo app at the web entry; browsers reach
the workspace through the ingress that forwards the workspace's origin to
`9780`. The process serves the routes on `8790` and the relay on `9780`, both
plain HTTP behind that ingress, which ends TLS.

The pages' live plane (live cursors, presence, window streams) is reached over
iroh, which a page reaches through a packet relay over websockets: the process
runs its own on `TILO_ROOMS_LIVE`, and pages dial it. An ingress that publishes
it on its own host names that origin in `TILO_ROOMS_LIVE_ORIGIN`, so the
address it forwards to never has to be the HTTPS origin's host.

## The deployment's environment

`entrypoint.sh` turns it into the host's flags and refuses any other
`TILO_ROOMS_*` with exit status 2, so a deployment that still sets what this
image no longer reads fails at start instead of running with a silent half of
its settings. Required:

| variable | what it is |
| --- | --- |
| `TILO_ROOMS_PERSISTENT` | the workspace's name; its state is `$TILO_ROOMS_STATE/$TILO_ROOMS_PERSISTENT` |
| `TILO_ROOMS_ROOT_FILE` | the workspace's owner root, 64 hex characters, from the operator's secrets |
| `TILO_ROOMS_RELAY_ORIGIN` | the `https://` origin the ingress publishes: pages, avatars and the callback |
| `TILO_ROOMS_DESCRIPTION` | what the public directory says the workspace is; listed, anyone joins through its door |
| `TILO_ROOMS_WEB_ENTRY` | the Tilo app's web entry the links the host mints open |

Optional, with these defaults:

| variable | default | what it is |
| --- | --- | --- |
| `TILO_ROOMS_NAME` | the name, capitalized | the name people see |
| `TILO_ROOMS_DEFAULT_ROLE` | `editor` | the role everyone who joins through the door gets |
| `TILO_ROOMS_CHAT_CHANNELS` | `general` | comma-separated channels the Chat opens with |
| `TILO_ROOMS_FEEDBACK_CHANNEL` | none | the channel members report problems to |
| `TILO_ROOMS_SEED_APPS` | `so.tilo.chat` | comma-separated apps authored at the first start |
| `TILO_ROOMS_AUTHOR_APPS` | every first-party app | comma-separated apps members write through; `none` makes the workspace read-only |
| `TILO_ROOMS_BOTS` | none | `;`-separated bot specs (`<program or directory entry> key=value …`) |
| `TILO_ROOMS_INACTIVE_AFTER_DAYS` | `30` | days without a session before a member stops being one |
| `TILO_ROOMS_OWN_WORKSPACES` | none | gigabytes each person may keep in the workspace of their own the host gives anyone who asks; unset gives none |
| `TILO_ROOMS_STATE` | `/run/tilo-rooms` | the volume the workspace lives on |
| `TILO_ROOMS_SITE_ORIGINS` | none | comma-separated origins besides the relay's that may join |
| `TILO_ROOMS_RELAY_PLAIN` | `0.0.0.0:9780` | the listener the ingress forwards the workspace to |
| `TILO_ROOMS_LIVE` | `0.0.0.0:9782` | the address the packet relay binds, as the ingress forwards to it |
| `TILO_ROOMS_LIVE_ORIGIN` | the address it bound | the `https://host[:port]` origin pages dial the packet relay at |
| `TILO_ROOMS_HTTP` | `0.0.0.0:8790` | the listener the routes are served on |
| `TILO_ROOMS_FETCH_PROXY` | on | `0` turns the fetch proxy off |
| `TILO_ROOMS_MODERATION_KEY_FILE` | `/run/secrets/moderation-model-key` | the model key the moderation agent asks with |
| `TILO_ROOMS_MODERATION_MODEL` | the relay's | the model it asks |
| `TILO_ROOMS_DISCORD_CLIENT_ID` | none | the Discord application members connect, once sign-in is set up |
| `TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE` | `/run/secrets/discord-client-secret` | its secret |
| `TILO_ROOMS_DISCORD_REDIRECT_URL` | `$TILO_ROOMS_RELAY_ORIGIN/discord/callback` | where Discord sends the browser back |
| `TILO_ROOMS_DISCORD_AUTHORIZE_URL` | `https://discord.com/oauth2/authorize` | Discord's consent page |
| `TILO_ROOMS_DISCORD_API_BASE` | `https://discord.com` | where Discord's API is, for a test server |
| `TILO_ROOMS_DISCORD_TOKEN_FILE` | `/run/secrets/discord-bot-token` | the bridge's bot token, read when the file is there |

`TILO_SOURCE_REVISION` is the image's own revision, which `/mode` reports;
`TILO_SECRETS_DIR` already points into the state volume, where the workspace
root is kept sealed. The relay's session limits
(`TILO_RELAY_MAX_WEB_SESSIONS`, `TILO_RELAY_MAX_HANDSHAKES`,
`TILO_RELAY_MAX_ROOM_GRANTS`) are the image's defaults and the deployment's to
raise.

## State on the volume

```
$TILO_ROOMS_STATE/$TILO_ROOMS_PERSISTENT/
  relay/         the workspace's own state: identity, host key, records,
                 bots, blobs, seeded apps
  room-id        the workspace's UUID, kept across restarts
  avatars/       avatars the host serves at /avatars/<name>
  secrets/       this host's secret store (TILO_SECRETS_DIR), sealed files
```

The directory layout is the one the wrapper this image replaces left on the
volume: a migrated deployment keeps its workspace, its identity and its bot
state.

The arrangement is the workspace's shared desktop record, so the host has no
layout flag. `TILO_ROOMS_LAYOUT` is still read and not passed on, so a
deployment that still names one (eu1 does) starts unchanged.

## Building

`tools/ci/publish/lounge-binaries.sh` builds the programs and
`tools/ci/publish/lounge-context.sh` lays out the build context; the
`publish-site` workflow (dispatch-only, main) builds this image and the web
entry from one commit, tests both and publishes `dev-<commit>`. Those steps
are also t's checks — `lounge-binaries`, `image-lounge-context`,
`image-lounge` — so a change is built by naming them.
`test-entrypoint.sh` asserts the environment-to-flags mapping and
`test-image.sh` what the built image must carry.

## Using it before it is published

`test-integration.sh` runs the image the way the deployment does and uses it:
the entrypoint reads production-shaped `TILO_ROOMS_*` on this machine's own
ports, the web entry is served beside it, and two headless Chromium browsers
open it, join the Lounge through its door as two people, chat, share a desk
and place a call they hear each other on, seeing each other present through
the host's own packet relay. It is the gate `publish-site` runs before
publishing, so a dispatched build publishes nothing a person cannot use.

```
bash os/containers/tilo-lounge/test-integration.sh \
  out/lounge/image.oci.tar out/web-desktop/bundle
```

The same gate is t's, and runs on any commit and branch: `t test lounge-gate`
builds the image and the web bundle it needs (each step its own cached check)
and runs it. What the gate found stays in `/target/lounge-gate` — every page's
snapshot and console, the site's and the Lounge's logs — which the check keeps
as its outputs, and what the pages said reaches the run's log as they fail.

It needs buildah (it starts the image chrooted, as `test-image.sh` does),
node, a headless Chrome (`CHROME`, else Playwright's cache), curl and a Rust
toolchain (the image's own `tilo-crypto` mints the site's certificate;
`TILO_CRYPTO_BIN` names a built one instead), and the artifacts
`publish-site` builds: `lounge-binaries.sh` +
`lounge-context.sh` + `publish/oci-build.sh` for the image, and
`web-desktop-bundle.sh wasm` + `web-desktop-bundle.sh bundle` for the bundle.
