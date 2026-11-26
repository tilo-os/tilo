# Tilo Lounge

One always-on public Tilo workspace, hosted by one process: `tilo-server`'s
persistent public workspace, the deployment routes beside it, and the Discord
bridge whose bot programs it runs.

```
GET /mode                 {"server":"up","revision":"<commit>"}   the readiness probe
GET /avatars/<name>       an avatar a bridged author published (immutable)
GET /discord/callback     Discord's OAuth redirect: links the account, then
                          sends the browser back to the web entry with its
                          one-time link secret
anything else             404; anything but GET     405
```

The workspace's own pages are the Tilo app at the web entry; browsers reach
the workspace through the ingress that forwards the workspace's origin to
`9780`. The process serves the routes on `8790` and the server on `9780`, both
plain HTTP behind that ingress, which ends TLS.

Pages reach the host over WebTransport, the browser's own transport: their
session, certification and the live session (live cursors, presence, window
streams). It is HTTP/3 on UDP, which no HTTPS ingress carries, so the process
listens on `TILO_WORKSPACES_LINK` and pages open their sessions at
`TILO_WORKSPACES_LINK_ORIGIN`, a name that leads straight to that UDP port. The
host presents its TLS certificate there, so it must be valid for that name;
with the development certificate, pages pin it by the hash the host
publishes instead.

Native devices reach the host over iroh, through a packet relay where they
have no direct path: the process runs its own on `TILO_WORKSPACES_LIVE`. An
ingress that publishes it on its own host names that origin in
`TILO_WORKSPACES_LIVE_ORIGIN`, so the address it forwards to never has to be the
HTTPS origin's host.

## The deployment's environment

`entrypoint.sh` turns it into the host's flags and refuses any other
`TILO_WORKSPACES_*` with exit status 2, so a deployment that still sets what this
image no longer reads fails at start instead of running with a silent half of
its settings. Required:

| variable | what it is |
| --- | --- |
| `TILO_WORKSPACES_PERSISTENT` | the workspace's name; its state is `$TILO_WORKSPACES_STATE/$TILO_WORKSPACES_PERSISTENT` |
| `TILO_WORKSPACES_ROOT_FILE` | the workspace's owner root, 64 hex characters, from the operator's secrets |
| `TILO_WORKSPACES_SERVER_ORIGIN` | the `https://` origin the ingress publishes: pages, avatars and the callback |
| `TILO_WORKSPACES_DESCRIPTION` | what the public directory says the workspace is; listed, anyone joins through its entry |
| `TILO_WORKSPACES_WEB_ENTRY` | the Tilo app's web entry the links the host mints open |

Optional, with these defaults:

| variable | default | what it is |
| --- | --- | --- |
| `TILO_WORKSPACES_NAME` | the name, capitalized | the name people see |
| `TILO_WORKSPACES_DEFAULT_ROLE` | `editor` | the role everyone who joins through the entry gets |
| `TILO_WORKSPACES_CHAT_CHANNELS` | `general` | comma-separated channels the Chat opens with; empty starts the workspace without its Chat |
| `TILO_WORKSPACES_FEEDBACK_CHANNEL` | none | the channel members report problems to |
| `TILO_WORKSPACES_PACKAGES` | the image's `/usr/local/share/tilo/packages` | a directory of signed `.tapp` packages, each installed as the version of its app the workspace runs |
| `TILO_WORKSPACES_BOTS` | none | `;`-separated bot specs (`<program or directory entry> key=value …`) |
| `TILO_WORKSPACES_PERSON_STORAGE_GB` | `10` | gigabytes per person, shared by the workspaces of their own the host gives anyone who asks |
| `TILO_WORKSPACES_STATE` | `/run/tilo-workspaces` | the volume the workspace lives on |
| `TILO_WORKSPACES_OWNERS` | none | comma-separated user public keys (64 hex characters) the workspace is started with as its owners: the people who let everyone else in, added at every start where they are not members yet. A deployment names its first ones here |
| `TILO_WORKSPACES_OPERATOR_SOCKET` | `$TILO_WORKSPACES_STATE/$TILO_WORKSPACES_PERSISTENT/operator.sock` | where the operator asks the host for what only it does: another host holding a workspace (`hold`, `unhold`). Everything about who is in the workspace is an action its owners and administrators take (`tilo workspace people add`, `tilo workspace people invite`), not something this socket does |
| `TILO_WORKSPACES_HOLD_FOR` | none | the workspaces of other people this host holds whoever's they are, when an owner has the host governing one hand it over (`tilo workspace hosts add`): `anyone`, or their ids, comma-separated. Without it the host holds those of the people it let in |
| `TILO_WORKSPACES_CALL_OWN_NETWORK` | off | `1` lets the server call addresses on its own network, not only the public internet: a model provider a person names, and the site of another server an owner has keep a workspace. For servers that share one network, as a local stack's do; never on a public server |
| `TILO_WORKSPACES_OPENAI_SIGN_IN` | `https://auth.openai.com` | Where people sign in to OpenAI to use their ChatGPT plan or have an API key issued; a local stack points it at its fake |
| `TILO_WORKSPACES_CHATGPT_BACKEND` | `https://chatgpt.com/backend-api/codex` | Where a signed-in ChatGPT plan is asked |
| `TILO_WORKSPACES_OPENAI_API` | `https://api.openai.com/v1` | Where an API key issued at an OpenAI sign-in is used |
| `TILO_WORKSPACES_SITE_ORIGINS` | none | comma-separated origins besides the relay's that may join |
| `TILO_WORKSPACES_SERVER_ORIGINS_ALSO` | none | comma-separated origins a front publishes this same server at besides `TILO_WORKSPACES_SERVER_ORIGIN` (a second name for it, on another network). The server makes an agent's setup code for a page that came by any origin it was told, and refuses one whose link names an origin it was not told, whatever a request calls the server |
| `TILO_WORKSPACES_SERVER_PLAIN` | `0.0.0.0:9780` | the listener the ingress forwards the workspace to |
| `TILO_WORKSPACES_LIVE` | `0.0.0.0:9782` | the address the packet relay binds, as the ingress forwards to it; `none` starts no packet relay of its own |
| `TILO_WORKSPACES_LIVE_ORIGIN` | the address it bound | the `https://name[:port]` origin devices dial the packet relay at; with `TILO_WORKSPACES_LIVE=none`, the packet relay elsewhere (such as `https://live-eu1.tilo.sh`) the server is reached through, which is how a server behind a router nobody dials into is reached |
| `TILO_WORKSPACES_TLS_CERT` | none | a publicly trusted certificate chain (PEM) the host's HTTPS listener presents instead of its own development one; read again when it changes |
| `TILO_WORKSPACES_TLS_KEY` | none | its private key (PEM); set with `TILO_WORKSPACES_TLS_CERT` |
| `TILO_WORKSPACES_LINK` | beside `TILO_WORKSPACES_WEB` | the UDP address the WebTransport listener binds, how pages reach the host |
| `TILO_WORKSPACES_LINK_ORIGIN` | the public origin | the `https://host:port` origin pages open WebTransport sessions at; set with `TILO_WORKSPACES_LINK` |
| `TILO_WORKSPACES_HTTP` | `0.0.0.0:8790` | the listener the routes are served on |
| `TILO_WORKSPACES_FETCH_PROXY` | on | `0` turns the fetch proxy off |
| `TILO_WORKSPACES_MODERATION_KEY_FILE` | `/run/secrets/moderation-model-key` | the model key the moderation agent asks with |
| `TILO_WORKSPACES_MODERATION_MODEL` | the server's | the model it asks |
| `TILO_WORKSPACES_DISCORD_CLIENT_ID` | none | the Discord application members connect, once sign-in is set up |
| `TILO_WORKSPACES_DISCORD_CLIENT_SECRET_FILE` | `/run/secrets/discord-client-secret` | its secret |
| `TILO_WORKSPACES_DISCORD_REDIRECT_URL` | `$TILO_WORKSPACES_SERVER_ORIGIN/discord/callback` | where Discord sends the browser back |
| `TILO_WORKSPACES_DISCORD_AUTHORIZE_URL` | `https://discord.com/oauth2/authorize` | Discord's consent page |
| `TILO_WORKSPACES_DISCORD_API_BASE` | `https://discord.com` | where Discord's API is, for a test server: the sign-in and the bridge's worker both reach it |
| `TILO_WORKSPACES_DISCORD_TOKEN_FILE` | `/run/secrets/discord-bot-token` | the bridge's bot token, read when the file is there |

`TILO_SOURCE_REVISION` is the image's own revision, which `/mode` reports;
`TILO_SECRETS_DIR` already points into the state volume, where the workspace
root is kept sealed. The server's session limits
(`TILO_SERVER_MAX_WEB_SESSIONS`, `TILO_SERVER_MAX_HANDSHAKES`,
`TILO_SERVER_MAX_WORKSPACE_GRANTS`) are the image's defaults and the deployment's to
raise.

## State on the volume

```
$TILO_WORKSPACES_STATE/$TILO_WORKSPACES_PERSISTENT/
  relay/         the workspace's own state: identity, host key, records,
                 bots, blobs, seeded apps
  workspace-id        the workspace's UUID, kept across restarts
  avatars/       avatars the host serves at /avatars/<name>
  secrets/       this host's secret store (TILO_SECRETS_DIR), sealed files
```

The directory layout is the one the wrapper this image replaces left on the
volume: a migrated deployment keeps its workspace, its identity and its bot
state.

The layout is the workspace's shared desktop record, so the host has no
layout flag. `TILO_WORKSPACES_LAYOUT` is still read and not passed on, so a
deployment that still names one (eu1 does) starts unchanged.

## Building

`tools/ci/publish/lounge-binaries.sh` builds the programs,
`tools/ci/publish/lounge-fonts.sh` the product font catalog and
`tools/ci/publish/lounge-context.sh` lays out the build context. t's
`lounge-binaries`, `lounge-fonts`, `image-lounge-context` and `image-lounge`
checks build and test this image; `image-web-entry-context`, `image-web-entry`
and `publish-web-entry` do the same for the web entry; `publish-lounge` and
`publish-web-entry` publish both from one commit, so a change is built by
naming the checks.
The checks that run the programs without an image around them (`e2e`)
ask `lounge-binaries.sh` for the profile Cargo.toml's
`lounge-check` names: the same code and optimisation level without the
whole-program link the image's own build spends.
`test-entrypoint.sh` asserts the environment-to-flags mapping and
`test-image.sh` what the built image must carry.

## Using it before it is published

`test-e2e.sh` is the web page end to end, on every change that reaches it
(`t test e2e`): the tilo.sh stack runs through production's own entrypoints
for this image, the API server and the web entry (`os/containers/tilo-sh`'s
`stack.sh`), from what the checks beside it built, and every flow runs at the
same time against that one site, relay and workspace, each as people of its own in
headless Chromium: the page booting, a setup through an agent and its
resumes, the startup identity approving a server key and the
visa run beside it, fresh visitors' entry into the Lounge, the wait list of a
entry that takes people by invitation, a stranger's onboarding on a tilo.sh of
its own (`test-onboarding.sh`: the wait list, the two mails, the invitation the
person who runs the host sends from the workspace's wait list, and the
workspace of their own with their agent set up in it, the passkey that brings
them back on a browser that forgot the site, a second workspace, and a visitor
still refused). Each flow runs alone too:

```
bash os/containers/tilo-lounge/test-e2e.sh out/web-entry/bundle /tmp/e2e boot
bash os/containers/tilo-lounge/test-onboarding.sh out/web-entry/bundle /tmp/onboarding
```

It needs node, a headless Chrome (`CHROME`, else Playwright's cache), curl,
bwrap, `TILO_SH_PROGRAMS`, `TILO_SH_LOUNGE`, `TILO_SH_SITE` and
`TILO_SH_CADDY` naming what `tilo-sh-binaries`, `image-web-entry-context` and
`tilo-sh-caddy` built, and `TILO_SERVER_BIN`, `TILO_CRYPTO_BIN`,
`TILO_FONTS_DIR` and `TILO_CLI_BIN` naming built programs (the
`lounge-check-binaries`, `lounge-check-tools`, `lounge-fonts` and
`lounge-check-cli` outputs), which the wait list's own Lounge and the setups
run. Every wait in the page drivers is a
number of steps of `TILO_STEP_SECONDS` (5 s by default; `test-e2e.sh` sets
20 s for a node that draws every page in software), so a hang fails within its
few steps and names its step.
