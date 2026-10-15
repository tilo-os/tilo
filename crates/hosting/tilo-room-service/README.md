# Short-lived shared rooms

`tilo-room-service` gives a website throwaway Tilo rooms that a visitor's
page and their phone join as certified devices. Each room is one
`tilo-relay room` process (feature `web`) with its own throwaway user root,
serving `https://<room>.<domain>`. It certifies a browser only once this
service has spent a single-use ticket on the pairing code that browser shows;
the browser keeps a non-extractable device key and never a root. When the
room expires, its relay exits or breaks the operator protocol, the relay is
stopped and its directory removed; nothing is kept.

The service and each room relay speak `tilo_relay_protocol::room_operator`:
typed JSON commands on the relay's stdin, typed events behind a
`tilo-room-operator ` prefix on its stdout, starting with the protocol
version. An unknown or malformed line on either side is an error, never
skipped. Both binaries ship in one image, so the protocol changes in one
commit.

The room carries only signed document commits, certification and control
messages. Every browser runs the browser runtime's desktop bundle (the
runtime with its certified peer and every web app) and renders everything
itself; the page seeds its own desktop and documents.

## API (plain HTTP, `TILO_ROOMS_API`)

| request | answer |
|---|---|
| `POST /rooms` | `201 {roomId, origin, room, userPublicKey, expiresAt, pageTicket, joinUrl, admit, events, invitations, app}`; `503` when all rooms are in use |
| `POST /rooms/<id>/invitations?key=<owner key>` `{record?}` | `201 {joinUrl, ticket}`: one more guest, whose link opens `record` (else the room's Writer); `429` beyond `TILO_ROOMS_MAX_GUESTS`. The body may be empty. Owner only: see below |
| `POST /rooms/<id>/admit` `{ticket, pairingCode}` | `200 {label, device, pairingCode, at}` once the room certified that browser; `403 {error}` for a spent, unknown or busy ticket, or when the room certified nobody for that code (no browser showed it within a minute, more than one did, or the policy refused it), which leaves the ticket usable |
| `GET /rooms/<id>/events?key=<owner key>` | Server-sent events: `member` per certified device, earlier ones replayed first; `expired` when the room ends. Owner only: see below |
| `GET /app/<path>` | the browser runtime bundle (page modules, `pkg/`, fonts, data), `rooms.mjs` and `page.html` |
| `GET /open/<path>` | the same bundle as the web entry guest links name: `index.html` (desktop), `app/index.html` (full tab) |
| `GET /open/entry.json` | `{tickets}`: where the entry pages redeem a link's guest ticket (`<api>/rooms/`) |
| `GET /healthz` | `{ok: true}` |

Minting invitations and watching members belong to whoever created the
room. `POST /rooms` answers `invitations` and `events` as complete URLs that
carry the room's owner key, 32 random bytes in hex, in the query (an
`EventSource` cannot send headers); pages use them as given and never build
them. Everything a guest link shows is public: the room's host is its
service id and the link names the room's uuid. So neither route answers
without the key, a wrong key or the uuid in place of the id reads as `404 no
such room`, and a guest link grants exactly its one guest's admission.
`admit` takes no key: it spends a single-use ticket on the pairing code a
browser shows, and accepts the room's id or its uuid.

`joinUrl` is a link in the web link grammar
(docs/architecture/web-routing.md):
`<api>/open/app/#/@<relay host>/w/<room>/so.tilo.word-processor[/<record>]?ticket=…`.
The entry page reads the room's root key from the relay, shows the pairing
code and posts `{ticket, pairingCode}` to `<tickets><room>/admit`, then opens
Writer full screen. The fragment never reaches a server, and the page strips
the ticket from the address bar before anything else runs. The entry is
`/open/` and not `/app/` because the grammar's `app/` segment means "full
tab", so no entry may end in it.

`web/rooms.mjs` is the page side, next to the bundle's `host.mjs` and
`relay.mjs`: `createRoom`, `joinRoom`, `recordOf`, `createInvitation`,
`watchMembers` (`useRegistry()` is kept for older pages and does nothing:
the bundle carries every app). `web/page.html` does with a room what the site's page does
(for review); the phone opens its guest link on the bundle's entry pages. The peer `joinRoom`
returns joins the room's live plane too: its mounts publish this device's
caret, selection and pointer and draw the other members' with their labels,
and `peer.live` (`state`, `memberCount`, `remoteState()`, `onMember`, …)
exposes it to the page.

## Configuration (environment)

| variable | default | |
|---|---|---|
| `TILO_ROOMS_API` | `0.0.0.0:8790` | API listener |
| `TILO_ROOMS_PUBLIC_API` | `http://localhost:8790` | the API's public URL (join links, `/app/`) |
| `TILO_ROOMS_SITE_ORIGINS` | `http://localhost:4321` | page origins that call the API and the rooms, comma-separated |
| `TILO_ROOMS_TLS` | `0.0.0.0:9443` | TLS front for `<room>.<domain>` |
| `TILO_ROOMS_DOMAIN` | `localhost` | rooms are `<room>.<domain>` |
| `TILO_ROOMS_PUBLIC_PORT` | none (443) | port in room origins when the front is not reached on 443 |
| `TILO_ROOMS_TLS_CERT`, `TILO_ROOMS_TLS_KEY` | self-signed | PEM chain and key for `*.<domain>` |
| `TILO_ROOMS_RELAY_BIN` | `/usr/local/bin/tilo-relay` | built with `--features web` |
| `TILO_ROOMS_ASSETS` | `/srv/tilo-room-assets` | `/app/`: see Build and run |
| `TILO_ROOMS_STATE` | `/run/tilo-rooms` | room directories; use a memory volume |
| `TILO_ROOMS_MAX` | `8` | rooms at once (1–64) |
| `TILO_ROOMS_TTL_SECS` | `1800` | room lifetime |
| `TILO_ROOMS_MAX_GUESTS` | `4` | guest tickets per room besides the page's |
| `TILO_ROOMS_PORT_BASE` | `9700` | ports of room relays (up to 64 from here) |
| `TILO_ROOMS_DIRECT_PORTS` | `0` | `1`: each room serves browsers on its own TCP and UDP port |
| `TILO_ROOMS_LOCAL` | `0` | `1`: local review in a plain browser (see below) |

Three ways to reach a room:

- **Direct ports** (`TILO_ROOMS_DIRECT_PORTS=1`): room `<id>` is
  `https://<id>.<domain>:<port>`, served by its relay on `0.0.0.0:<port>`
  over TCP (HTTPS) and UDP (WebTransport, for the live plane) with the
  `*.<domain>` certificate, or without one the relay's development
  certificate. Expose the port range for TCP and UDP.
- **TLS front** (default): `https://<id>.<domain>[:TILO_ROOMS_PUBLIC_PORT]`.
  The front terminates the browser's TLS with the `*.<domain>` certificate
  and relays the bytes over a second TLS connection to the room's relay,
  which presents a certificate the service generated at start and pins. The
  ingress must pass `*.<domain>` through by SNI. It carries TCP only.
- **Local** (`TILO_ROOMS_LOCAL=1`, development only): room `<id>` is
  `http://localhost:<port + 64>`, served by its relay's plain listener on
  `127.0.0.1` (`tilo-relay room --web-plain`). A page on
  `http://localhost:4321` needs no certificate exception, host mapping or
  browser flag. The relay's TLS listener stays on `127.0.0.1:<port>` with
  its development certificate: pages try WebTransport there (pinned by
  hash) and otherwise carry the live plane over the HTTPS stream fallback on
  the plain origin, so cursors and carets still arrive, a little later.

In every mode the live plane (pointers, carets, selections, typing
previews) prefers WebTransport and falls back to the HTTPS stream routes
when UDP or WebTransport is unavailable; documents sync over HTTPS either
way.

A page publishes one presence per device, from the mount the person acted
in last. An agent the page runs (a mount with `agent`, or
`peer.agentLive(agent)`) publishes its own caret under
`/live/grant/agent`: a namespace derived from the room, the device and the
agent id, labelled as the agent.

### WebTransport: what it takes

WebTransport is QUIC, so browsers need **UDP** to the relay's port; the
TCP port alone only carries the fallback.

- **Production:** expose the room ports for TCP *and* UDP. The
  `tilo-rooms-direct` LoadBalancer carries both on each port (a load
  balancer without mixed protocols needs a second Service for UDP), and
  firewalls and security groups must pass inbound UDP on them. The relays
  present the public `*.<domain>` certificate, so browsers need no pinned
  hash. Visitors whose network drops UDP get the fallback over the same TCP
  port.
- **Locally, on Windows with WSL2:** WSL's default NAT networking forwards
  only TCP from Windows' `localhost`, so Windows browsers always get the
  fallback. For WebTransport:
  1. Switch WSL to mirrored networking (`%UserProfile%\.wslconfig`:
     `[wsl2]` `networkingMode=mirrored`, then `wsl --shutdown`). Windows
     and WSL then share `localhost` for TCP and UDP.
  2. Allow inbound UDP on the relay ports through the Hyper-V firewall
     (elevated PowerShell): `New-NetFirewallHyperVRule -Name tilo-rooms-udp
     -DisplayName "Tilo rooms (UDP)" -Direction Inbound -VMCreatorId
     '{40E0AC32-46A5-438A-A0B2-2B479E8F2E90}' -Protocol UDP -LocalPorts
     9700-9763`. For phones on the LAN, also allow those ports in Windows
     Defender Firewall (`New-NetFirewallRule -Direction Inbound -Protocol UDP
     -LocalPort 9700-9763 -Action Allow`).
  3. The page then reaches `https://localhost:<port>/moq/live` over UDP. The
     relay's development certificate is pinned by the hash the relay
     announces (`serverCertificateHashes`); no certificate exception is
     needed.

## Persistent mode

`TILO_ROOMS_PERSISTENT=<name>` runs a governed public room that survives
restarts instead of throwaway rooms; `POST /rooms` is then disabled. It is a
public workspace anyone can open. Its id is kept in
`TILO_ROOMS_STATE/<name>/room-id` and the child relay's state beside it, so
`TILO_ROOMS_STATE` must be a persistent volume. People see the room as
`TILO_ROOMS_NAME` (else `<name>` with a capital first letter): the relay
names its workspace and its Chat workspace so at every start, renaming a
room created under another name.
`TILO_ROOMS_CHAT_CHANNELS` (for example `general,help`) are the channels
Chat starts with while the room's workspace has none, each with the id
`c_<name>` (`-` becomes `_`) so a bridged Discord server maps onto the same
channels on every start. Without it no channels are made, and a chat that
has channels keeps exactly those.
`TILO_ROOMS_STRIPS` (for example `Main,Games,Quiet`) are the strips, the
named places people gather in, of the room's shared desktop: each is added
to its first workspace at start where none has that name, so the first
person in finds them in the top bar. The strips people make are kept.

The service is the room's API and relay, never a page: the Tilo app lives
at its web entry (`TILO_ROOMS_WEB_ENTRY`, e.g. `https://app.example/`), which
opens the room as a workspace. `GET /` redirects there, or answers 404
without a web entry. Admission runs in the app: each visitor is their own
person with their own identity, `POST /join` pre-admits it (per-IP join
windows, an online cap, a cooldown for addresses of removed members) and
the browser redeems the bound invitation. A read-only embed joins as the
room's spectator through `POST /spectate`. `GET /presence` answers
`{online, watching}`. The owner root comes from a mounted secret; without
it the service refuses to start. The full design is in
`docs/architecture/discord-bridge.md` §9.

**One origin.** The room API and the relay share one public origin
(e.g. `https://rooms.example`). The relay serves browsers on its plain HTTP
listener (`TILO_ROOMS_RELAY_PLAIN`) and every relay route starts with
`/peer/` (`/peer/v1/…`, the live fallback's `/peer/v3/live/…`), so the
ingress sends that prefix to the relay and everything else to the service
(`TILO_ROOMS_API`); nothing the service answers starts with `/peer/`. Set
`TILO_ROOMS_PUBLIC_API` and `TILO_ROOMS_RELAY_ORIGIN` to that origin.
WebTransport needs UDP to the relay, which an HTTPS ingress does not carry,
so the live plane uses the HTTPS stream fallback there.
`TILO_ROOMS_SITE_ORIGINS` lists every page origin that opens the room
(e.g. `https://app.example,https://site.example`): they may call the API, the relay
accepts them, and they may frame the service's pages.

| Variable | Default | Meaning |
|---|---|---|
| `TILO_ROOMS_PERSISTENT` | unset | the room's name; turns persistent mode on |
| `TILO_ROOMS_NAME` | `<name>`, capitalized | the name people see for the room (1–80 characters) |
| `TILO_ROOMS_CHAT_CHANNELS` | unset: none | channels Chat starts with while it has none, comma-separated names of lowercase letters, digits and `-` |
| `TILO_ROOMS_STRIPS` | unset: none | strips the shared desktop has, comma-separated names of up to 48 characters |
| `TILO_ROOMS_RELAY_ORIGIN` | required | the relay's public origin, e.g. `https://rooms.example` |
| `TILO_ROOMS_RELAY_PLAIN` | `0.0.0.0:9780` | the relay's plain listener the ingress forwards `/peer/` to |
| `TILO_ROOMS_WEB_ENTRY` | none | the Tilo app's web entry (e.g. `https://app.example/`), announced by the relay so `share-link` and bots mint links that open the room there; without it the room's resources cannot be shared as links |
| `TILO_ROOMS_LIVE_ORIGIN` | unset | an `https://` origin (port allowed), e.g. `https://live.rooms.example` with UDP 443 forwarded to the listener, that reaches the relay's QUIC listener directly, for an ingress that passes HTTP only (a CDN): browsers try WebTransport there first and fall back within 2 s, silently, to the HTTPS stream through the ingress |
| `TILO_ROOMS_LIVE_LISTEN` | `0.0.0.0:9781` | that listener: WebTransport on UDP (HTTPS on TCP too, unused) |
| `TILO_ROOMS_LIVE_TLS_CERT`, `TILO_ROOMS_LIVE_TLS_KEY` | `/run/live-tls/tls.crt`, `/run/live-tls/tls.key` | its PEM certificate and key for the live origin's name; re-read within 30 s of a renewal, no restart |
| `TILO_ROOMS_ROOT_FILE` | `/run/secrets/room-root` | 64 hex characters: the owner root |
| `TILO_ROOMS_TRUSTED_PROXIES` | none | CIDRs whose `X-Forwarded-For` is believed |
| `TILO_ROOMS_MAX_GUESTS` | `300` | people online at once (up to 10000; one room admitted 100 with join p99 under 10 s and stopped admitting past about 250 in the last capacity sweep, `docs/benchmarks/room-host-capacity-2026-10.md`) |
| `TILO_ROOMS_MAX_SPECTATORS` | `1000` | watching sessions at once |
| `TILO_ROOMS_GUEST_JOINS_PER_10_MIN`, `TILO_ROOMS_GUEST_JOINS_PER_DAY` | `5`, `30` | guest joins one address may make (an IPv6 address counts by its /64); a visitor past them watches until the window frees up. The windows are rebuilt from the guest ledger on the volume at start, so a restart does not reset them; a join the room never answered (it was restarting) does not count. Raise them where many visitors share an address: offices, schools, mobile carriers' NAT |
| `TILO_ROOMS_SPECTATES_PER_10_MIN` | `30` | spectator certificates one address may take |
| `TILO_ROOMS_OWNERS`, `TILO_ROOMS_MODERATORS` | none | user keys (hex), comma-separated |
| `TILO_ROOMS_SEED_APPS` | `so.tilo.chat,so.tilo.notes,so.tilo.canvas` | apps added to the room once, comma-separated; notes and canvas start with a welcome note and starter shapes in the room's default record |
| `TILO_ROOMS_BOTS` | none | bots installed at the first start, `;`-separated, each `<entry> key=value …`, where the entry is a directory bot or a program file such as the image's `/usr/local/share/tilo/bots/discord-bridge.json`; the Discord bridge takes `guild=<server id>`, optionally `channel=<channel id>` to bridge only that channel and `server=<name>`, and the host connects the room's Chat to that server before the bridge runs |
| `TILO_ROOMS_DISCORD_TOKEN_FILE` | `/run/secrets/discord-bot-token` if present | the Discord bot token, read only when bots run |
| `TILO_ROOMS_PRUNE_AFTER_MINS` | `60` | minutes (1–525600) a guest may go without a session before the relay removes them from the room, so the room does not slow with every visitor it ever had; a pruned guest who comes back joins through the door again as the same person, and their address is not cooled down. Owners and moderators are never pruned |
| `TILO_ROOMS_RESET_SECS` | `21600` | every this many seconds, snapshot the seeded notes and canvas (last 10 kept under `<state>/relay/snapshots/<app>/`) and reset them to their starting content; chat is never reset; `0` disables |

Every guest and every spectator holds a web session on the child relay, so
the service sizes the relay's caps from the room's: `TILO_RELAY_MAX_WEB_SESSIONS`
is guests plus spectators plus 64, `TILO_RELAY_MAX_HANDSHAKES` 1024,
`TILO_RELAY_MAX_LIVE_GRANTS` four and `TILO_RELAY_MAX_ROOM_GRANTS` sixteen
per session. Any of them set in the environment wins. The child also runs
with `MALLOC_ARENA_MAX=2` unless set, which halves its memory under load.

**Who is here.** Members' pages post their own recent actions (titles only)
to the relay's `/peer/v1/here` and show everyone's one-line status. The relay
always answers a deterministic line. Optionally it asks a model provider for
a short prose line, which is shown only while it cites actions the person
still has; otherwise the deterministic line stays. The child relay inherits
these variables. Without an endpoint, no model is asked. An invalid setting
logs a warning and leaves the lines deterministic; the room keeps serving.

| Variable | Default | Meaning |
|---|---|---|
| `TILO_RELAY_SUMMARY_ENDPOINT` | unset: off | the provider's base URL, e.g. `https://api.anthropic.com` |
| `TILO_RELAY_SUMMARY_PROTOCOL` | `anthropic` | `anthropic` or `openai-compatible` |
| `TILO_RELAY_SUMMARY_MODEL` | `claude-haiku-5-5` | the model id |
| `TILO_RELAY_SUMMARY_KEY_FILE` | unset | a file holding the API key, read once at start; required for `anthropic` |
| `TILO_RELAY_SUMMARY_MAX_TOKENS` | `200` | output tokens per call, 1–512 |
| `TILO_RELAY_SUMMARY_PER_HOUR` | `120` | calls in any rolling hour across the relay, 1–3600; after that lines stay deterministic until the hour rolls on |
| `TILO_RELAY_SUMMARY_MIN_INTERVAL_SECS` | `60` | least time between two calls for one person, 10–3600 |

A call is made only when a person's set of actions changed. Each person has
at most one call in flight, and the relay has at most two. A call that takes
longer than 15 s is dropped. Calls never hold up a reply.

## Build and run

The image is built by `.forgejo/workflows/room-service-image.yml`: the CI
builder compiles `tilo-relay` (feature `agents`), `tilo-room-service` and
the desktop bundle, and BuildKit assembles
`container/Containerfile` from those outputs (`bin/`, `assets/`). Pushes to
main and manual runs publish `registry.tilo.so/tilo/room-service:sha-<commit>`
and a digest receipt (`room-service-image-receipt`: repository, tag, digest,
source revision, platform and the build run); deployments pin the digest.
`container/test-image.sh` is the image's smoke test. Run it on a branch with
`fgj workflow run room-service-image.yml -R tilo/tilo --ref <branch>`.

Production pins this image and the web entry together, from one main
commit, in the infra repository. `tools/release/cloud_promotion.py` plans
that change: `plan` picks the newest main commit with both images (or
`--revision`, or two `--receipt` files), checks each digest's revision label
and platform in the registry and prints the exact manifest diff and pull
request body (`--infra <checkout> --apply` edits a checkout); `propose` opens
the pull request on a `promote/` branch with `INFRA_PROMOTION_TOKEN`; `drift`
reports which revision each service runs and how far main is ahead. The
`Release / Cloud promotion` workflow (`cloud-promotion.yml`) runs `drift`
every Monday and the promotion on a manual run. Promotions are batched: each
web entry deploy makes open tabs offer a reload.

Locally, `dev/run.py` runs the persistent room as production does: this
service in persistent mode with its child relay, and the web entry laid out
by the web entry image's `layout.py` with the `entry.json` that opens the
room, on plain `localhost` origins (`TILO_ROOMS_LOCAL=1`). It builds what it
needs or reuses prebuilt binaries and a bundle, prints the URL to open and
stops everything on Ctrl-C; `dev/run.py --help` describes the edit loop and
driving it with headless Chromium.

`dev/auth-e2e.mjs` checks getting in on that stack, end to end, in headless
Chrome: a fresh browser's first visit through the door, a reload, a second
and a new tab, a damaged link, another person's invitation (spent, and one
issued for someone else), and a host set up again: its stale link refused,
the offer to join it again declined and then accepted (one join, the same
person, the old host's state left untouched beside the new host's). It
starts `run.py` from prebuilt binaries and a bundle and writes
`summary.json` and `summary.md`; the `Browser / authentication` workflow
runs it nightly and on demand.

`dev/limits-e2e.py` checks the persistent room's enabled limits and what
it keeps, over plain HTTP with no browser or bundle: on scratch state with
small caps it fills the guest cap, the spectator cap and each per-address
join window exactly, checks every refusal's status, wording and
`Retry-After`, that a refused visitor holds no seat and that one told "full"
gets in once an unused seat's hold ends, the caps and refusal counters on
`/metrics`; then it restarts the service and checks the room, its spectator
person, the guest ledger and each address's join window survive, that each
seeded app keeps exactly its ten newest reset snapshots (chat none), and
that SIGTERM stops the service and its relay within 20 seconds (eu1's
grace period). The same workflow runs it before the browser check.

`dev/owner-key-e2e.py` checks a throwaway room's owner routes the same way:
it creates a room as a page does, then checks that nothing a guest link
shows (the room's host, its uuid, the link without its ticket), a wrong key
or the uuid with the right key mints an invitation or opens the member
stream, each refused as no such room and spending no guest slot; that the
answer's `invitations` and `events` URLs work as given; and that an invitee
still redeems their ticket at the room's uuid. It runs with `limits-e2e.py`.

```sh
python3 crates/hosting/tilo-room-service/dev/limits-e2e.py \
  --relay-bin target/release/tilo-relay --room-service-bin target/release/tilo-room-service \
  --out target/limits-e2e
python3 crates/hosting/tilo-room-service/dev/owner-key-e2e.py \
  --relay-bin target/release/tilo-relay --room-service-bin target/release/tilo-room-service \
  --out target/owner-key-e2e
node crates/hosting/tilo-room-service/dev/auth-e2e.mjs --bundle DIR \
  --relay-bin target/release/tilo-relay --room-service-bin target/release/tilo-room-service \
  --out target/auth-e2e [--allow-dev]
```

For short-lived rooms, serve the output of
`crates/browser/tilo-browser-runtime/scripts/build.sh DIR --bundle desktop`
plus `web/*` as `TILO_ROOMS_ASSETS`, and run `target/release/tilo-room-service`
with `TILO_ROOMS_RELAY_BIN=target/release/tilo-relay` and
`TILO_ROOMS_LOCAL=1`. `http://localhost:8790/app/page.html` then creates a
room in any browser; its phone link opens `/open/app/`. The site's dev server
uses it with `PUBLIC_TILO_ROOMS_URL=http://localhost:8790`.

## Operating it

- **`GET /metrics`** (Prometheus text, in-cluster only: requests carrying
  `X-Forwarded-For` get 404). `tilo_rooms_build_info{revision}` is 1 and
  names the commit the running image was built from (`TILO_SOURCE_REVISION`,
  which the Containerfile sets from the revision label; `unknown` outside an
  image), to compare with the revision of the pinned digest.
  `tilo_rooms_*`: rooms, online guests and
  spectators, the persistent room's caps (`max_guests`, `max_spectators`, so
  an alert divides by the running value rather than a copy of it),
  `joins_total`, `join_failures_total{reason}`, mode, browser
  error counters, and the relay's own counters read from the snapshot each
  relay persists every second (`relay_up`, `relay_sessions`,
  `relay_live_drops_total`, `relay_records_total`, `relay_record_bytes_total`,
  `relay_bytes_sent_total`). Other processes of the pod add their own by
  writing Prometheus text to `<state>/metrics.d/<name>.prom`; each file is
  appended with `tilo_rooms_textfile_age_seconds{file}` so a writer that
  stopped shows. The Discord bridge writes `bridge.prom` with
  `tilo_rooms_bridge_lag_seconds`, `tilo_rooms_bridge_queue_depth` and
  `tilo_rooms_bridge_rate_limited_total`.
- **`POST /errors`**: a page's uncaught error, at most 4 KiB, 20 a second in
  total; logged to stderr as one `{"event":"browser_error",...}` JSON line,
  never stored. The app's entry pages send them with `sendBeacon`.
- **Kill switch**: `tilo-room-service mode [normal|spectator-only|back-soon]`
  (run in the pod, with the same `TILO_ROOMS_STATE`) sets the mode and no
  argument prints it; the service notices within a second. `TILO_ROOMS_MODE`
  is the mode at start, the file wins once set. `spectator-only` refuses new
  rooms, invitations and admissions with `503` and `Retry-After`;
  `back-soon` also stops spectating (the persistent room's routes read
  `ops::get().mode()`). `GET /mode` tells the page which, and is public:
  `{"mode": "normal", "revision": "<commit>"}`, the same revision as the
  build info, so drift can be seen from outside the cluster.
