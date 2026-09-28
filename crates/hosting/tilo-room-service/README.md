# Short-lived shared rooms

`tilo-room-service` gives a website throwaway Tilo rooms that a visitor's
page and their phone join as certified devices: each browser keeps its own
non-extractable device key and the room's user root certifies it, exactly as
`tilo-relay dev` does, but only once the service has spent a single-use
ticket on the pairing code that browser shows. Each room is one
`tilo-relay room` process (feature `web`) on its own state directory with its
own throwaway user root, serving `https://<room>.<domain>`. When the room
expires, or its relay exits, the relay is stopped and its directory removed;
nothing is kept.

The page seeds the room's desktop itself (`mountDesktop({apps})`); the
service holds no documents.

## API (plain HTTP, `TILO_ROOMS_API`)

| request | answer |
|---|---|
| `POST /rooms` | `201 {roomId, origin, room, userPublicKey, expiresAt, pageTicket, joinUrl, admit, events, invitations, app}`; `503` when all rooms are in use |
| `POST /rooms/<id>/invitations` | `201 {joinUrl, ticket}`: one more guest; `429` beyond `TILO_ROOMS_MAX_GUESTS` |
| `POST /rooms/<id>/admit` `{ticket, pairingCode}` | `200 {label, device, pairingCode, at}` once the room certified that browser; `403` for a spent or unknown ticket |
| `GET /rooms/<id>/events` | Server-sent events: `member` per certified device, earlier ones replayed first; `expired` when the room ends |
| `GET /app/<path>` | the browser desktop bundle and `rooms.mjs`, `join.html` |
| `GET /healthz` | `{ok: true}` |

`joinUrl` is `<api>/app/join.html#origin=…&user=…&room=…&ticket=…&admit=…`;
append `&record=<id>` to open that record full screen (`createInvitation`
in `rooms.mjs` does). The fragment never reaches a server.

`web/rooms.mjs` is the page side: `createRoom`, `joinRoom` (device custody,
`connectPeer`, admission of its pairing code), `whenReady`, `recordOf`,
`createInvitation`, `watchMembers`.

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
| `TILO_ROOMS_ASSETS` | `/srv/tilo-room-assets` | `build.sh --bundle desktop` output plus `web/` |
| `TILO_ROOMS_STATE` | `/run/tilo-rooms` | room directories; use a memory volume |
| `TILO_ROOMS_MAX` | `8` | rooms at once (1–64) |
| `TILO_ROOMS_TTL_SECS` | `1800` | room lifetime |
| `TILO_ROOMS_MAX_GUESTS` | `4` | guest tickets per room besides the page's |
| `TILO_ROOMS_PORT_BASE` | `9700` | loopback ports of room relays |

The TLS front terminates the browser's TLS with the `*.<domain>` certificate
and relays the bytes over a second TLS connection to the room's relay, which
presents a certificate the service generated at start and pins. The ingress
must pass `*.<domain>` through by SNI. WebTransport (UDP) is not relayed, so
the live plane (cursors, presence) uses its HTTPS stream fallback.

## Build and run

```sh
crates/hosting/tilo-room-service/scripts/build-image.sh /abs/context code.tilo.so/tilo/room-service:REV

# Locally: rooms at https://<room>.localhost:9443 (*.localhost resolves to
# loopback; the certificate is self-signed).
TILO_ROOMS_PUBLIC_PORT=9443 TILO_ROOMS_ASSETS=/abs/context/assets \
TILO_ROOMS_RELAY_BIN=$PWD/target/release/tilo-relay TILO_ROOMS_STATE=/tmp/tilo-rooms \
  target/release/tilo-room-service
```
