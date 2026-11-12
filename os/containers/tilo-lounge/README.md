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

Pages reach the host over WebTransport, the browser's own transport: their
session, certification and the live plane (live cursors, presence, window
streams). It is HTTP/3 on UDP, which no HTTPS ingress carries, so the process
listens on `TILO_ROOMS_LINK` and pages open their sessions at
`TILO_ROOMS_LINK_ORIGIN`, a name that leads straight to that UDP port. The
host presents its TLS certificate there, so it must be valid for that name;
with the development certificate, pages pin it by the hash the host
publishes instead.

Native devices reach the host over iroh, through a packet relay where they
have no direct path: the process runs its own on `TILO_ROOMS_LIVE`. An
ingress that publishes it on its own host names that origin in
`TILO_ROOMS_LIVE_ORIGIN`, so the address it forwards to never has to be the
HTTPS origin's host.

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
| `TILO_ROOMS_PERSON_STORAGE_GB` | `10` | gigabytes per person, shared by the workspaces of their own the host gives anyone who asks |
| `TILO_ROOMS_STATE` | `/run/tilo-rooms` | the volume the workspace lives on |
| `TILO_ROOMS_SITE_ORIGINS` | none | comma-separated origins besides the relay's that may join |
| `TILO_ROOMS_RELAY_PLAIN` | `0.0.0.0:9780` | the listener the ingress forwards the workspace to |
| `TILO_ROOMS_LIVE` | `0.0.0.0:9782` | the address the packet relay binds, as the ingress forwards to it |
| `TILO_ROOMS_LIVE_ORIGIN` | the address it bound | the `https://host[:port]` origin devices dial the packet relay at |
| `TILO_ROOMS_TLS_CERT` | none | a publicly trusted certificate chain (PEM) the host's HTTPS listener presents instead of its own development one; read again when it changes |
| `TILO_ROOMS_TLS_KEY` | none | its private key (PEM); set with `TILO_ROOMS_TLS_CERT` |
| `TILO_ROOMS_LINK` | beside `TILO_ROOMS_WEB` | the UDP address the WebTransport listener binds, how pages reach the host |
| `TILO_ROOMS_LINK_ORIGIN` | the public origin | the `https://host:port` origin pages open WebTransport sessions at; set with `TILO_ROOMS_LINK` |
| `TILO_ROOMS_HTTP` | `0.0.0.0:8790` | the listener the routes are served on |
| `TILO_ROOMS_FETCH_PROXY` | on | `0` turns the fetch proxy off |
| `TILO_ROOMS_MODERATION_KEY_FILE` | `/run/secrets/moderation-model-key` | the model key the moderation agent asks with |
| `TILO_ROOMS_MODERATION_MODEL` | the relay's | the model it asks |
| `TILO_ROOMS_DISCORD_CLIENT_ID` | none | the Discord application members connect, once sign-in is set up |
| `TILO_ROOMS_DISCORD_CLIENT_SECRET_FILE` | `/run/secrets/discord-client-secret` | its secret |
| `TILO_ROOMS_DISCORD_REDIRECT_URL` | `$TILO_ROOMS_RELAY_ORIGIN/discord/callback` | where Discord sends the browser back |
| `TILO_ROOMS_DISCORD_AUTHORIZE_URL` | `https://discord.com/oauth2/authorize` | Discord's consent page |
| `TILO_ROOMS_DISCORD_API_BASE` | `https://discord.com` | where Discord's API is, for a test server: the sign-in and the bridge's worker both reach it |
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

`tools/ci/publish/lounge-binaries.sh` builds the programs,
`tools/ci/publish/lounge-fonts.sh` the product font catalog and
`tools/ci/publish/lounge-context.sh` lays out the build context; the
`publish-site` workflow (dispatch-only, main) builds this image and the web
entry from one commit, tests both and publishes `dev-<commit>`. Those steps
are also t's checks — `lounge-binaries`, `lounge-fonts`,
`image-lounge-context`, `image-lounge` — so a change is built by naming them.
The checks that run the programs without an image around them (`e2e`,
`page-tour`) ask `lounge-binaries.sh` for the profile Cargo.toml's
`lounge-gate` names: the same code and optimisation level without the
whole-program link the image's own build spends.
`test-entrypoint.sh` asserts the environment-to-flags mapping and
`test-image.sh` what the built image must carry.

## Using it before it is published

`test-e2e.sh` is the web page end to end, on every change that reaches it
(`t test e2e`): the tilo.sh stack runs through production's own entrypoints
for this image, the front door and the web entry (`os/containers/tilo-sh`'s
`stack.sh`), from what the checks beside it built, and every flow runs at the
same time against that one site, relay and room, each as people of its own in
headless Chromium: the page booting, two people chatting and calling, the
desk's menus and gestures, an invitation, a shared link, a setup through an
agent and its resumes, the startup identity approving a server key and the
visa run beside it, fresh visitors' entry into the Lounge, the wait list of a
door that takes people by invitation, a stranger's whole onboarding on a host
of their own (`test-onboarding.sh`: the wait list, the two mails, the
invitation a person of the host sends from the workspace's wait list, the
workspace of their own they land in, their passkey, a second workspace, and a
visitor who still cannot get in), and the chat read back after the relay
restarts. Each flow runs alone too:

```
bash os/containers/tilo-lounge/test-e2e.sh out/web-desktop/bundle /tmp/e2e people-call
bash os/containers/tilo-lounge/test-onboarding.sh out/web-desktop/bundle /tmp/onboarding
```

It needs node, a headless Chrome (`CHROME`, else Playwright's cache), curl,
bwrap, `TILO_SH_PROGRAMS`, `TILO_SH_LOUNGE`, `TILO_SH_SITE` and
`TILO_SH_CADDY` naming what `tilo-sh-binaries`, `image-web-entry-context` and
`tilo-sh-caddy` built, and `TILO_RELAY_BIN`, `TILO_CRYPTO_BIN`,
`TILO_FONTS_DIR` and `TILO_CLI_BIN` naming built programs (the
`lounge-gate-binaries`, `lounge-gate-tools`, `lounge-fonts` and
`lounge-gate-cli` outputs), which the wait list's own Lounge and the setups
run. Every wait in the page drivers is a
number of steps of `TILO_STEP_SECONDS` (5 s by default; `test-e2e.sh` sets
20 s for a node that draws every page in software), so a hang fails within its
few steps and names its step. That every app the page offers opens from the
search on the next frame is an in-process test of the desk's search
(`crates/session/desktop-surface/tests/every_web_app_opens_from_search.rs`),
not a browser's walk.
