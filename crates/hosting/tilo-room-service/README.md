# Short-lived shared rooms

`tilo-room-service` gives a website throwaway Tilo rooms that a visitor's
page and their phone join as certified devices. Each room is one
`tilo-relay room` process (feature `web`) with its own throwaway user root,
serving `https://<room>.<domain>`. It certifies a browser only once this
service has spent a single-use ticket on the pairing code that browser shows;
the browser keeps a non-extractable device key and never a root. When the
room expires, or its relay exits, the relay is stopped and its directory
removed; nothing is kept.

The room carries only signed document commits, certification and control
messages. Every browser runs the browser runtime's desktop bundle (the
runtime with its certified peer and every web app) and renders everything
itself; the page seeds its own desktop and documents.

## API (plain HTTP, `TILO_ROOMS_API`)

| request | answer |
|---|---|
| `POST /rooms` | `201 {roomId, origin, room, userPublicKey, expiresAt, pageTicket, joinUrl, admit, events, invitations, app}`; `503` when all rooms are in use |
| `POST /rooms/<id>/invitations` | `201 {joinUrl, ticket}`: one more guest; `429` beyond `TILO_ROOMS_MAX_GUESTS` |
| `POST /rooms/<id>/admit` `{ticket, pairingCode}` | `200 {label, device, pairingCode, at}` once the room certified that browser; `403` for a spent or unknown ticket |
| `GET /rooms/<id>/events` | Server-sent events: `member` per certified device, earlier ones replayed first; `expired` when the room ends |
| `GET /app/<path>` | the browser runtime bundle (page modules, `pkg/`, fonts, data), `rooms.mjs`, `join.html` and `page.html` |
| `GET /healthz` | `{ok: true}` |

`joinUrl` is `<api>/app/join.html#origin=…&user=…&room=…&ticket=…&admit=…`;
`createInvitation(room, {record})` in `rooms.mjs` appends `&record=<id>`,
which the join page opens full screen. The fragment never reaches a server.

`web/rooms.mjs` is the page side, next to the bundle's `host.mjs` and
`relay.mjs`: `createRoom`, `joinRoom`, `recordOf`, `createInvitation`,
`watchMembers` (`useRegistry()` is kept for older pages and does nothing:
the bundle carries every app). `web/page.html` does with a room what the site's page does
(for review); `web/join.html` is the phone's page. The peer `joinRoom`
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
| `TILO_ROOMS_ASSETS` | `/srv/tilo-room-assets` | `/app/`: see `scripts/build-image.sh` |
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

## Build and run

```sh
crates/hosting/tilo-room-service/scripts/build-image.sh /abs/context code.tilo.so/tilo/room-service:REV
```

Locally, serve the output of
`crates/browser/tilo-browser-runtime/scripts/build.sh DIR --bundle desktop`
plus `web/*` as `TILO_ROOMS_ASSETS`, and run `target/release/tilo-room-service`
with `TILO_ROOMS_RELAY_BIN=target/release/tilo-relay` and
`TILO_ROOMS_LOCAL=1`. `http://localhost:8790/app/page.html` then creates a
room in any browser; its phone link opens `join.html`. The site's dev server
uses it with `PUBLIC_TILO_ROOMS_URL=http://localhost:8790`.
