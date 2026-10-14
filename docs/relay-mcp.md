# MCP for hosted rooms

An external MCP client can use a room's semantic actions directly over the
relay's HTTPS listener, without running a compositor or a local MCP bridge:

```text
https://host.example/peer/v1/rooms/<canonical-room-uuid>/mcp
```

The endpoint reuses `tilo-runtime`'s MCP protocol and Streamable HTTP transport,
backed by the relay's existing app-serving plane. It is not a second app API:
resource discovery, action contracts, typed arguments, invocation receipts,
status and cancellation are the same action door other collaborators use.

## Host setup

Build `tilo-relay` with `--features agents` and run its app-serving host
(`--agents`) with the existing web listener configured. The `agents` feature
includes `mcp`. An embedding host can instead select `--features mcp` and supply
an `AppServingPlane` through `Relay::start_with_app_serving`.

Use the existing `--web`, `--tls files:<chain.pem>,<key.pem>` and `--public-host`
options for direct HTTPS. Behind a TLS-terminating ingress, use the existing
`--web-plain` and `--public-origin` configuration, restrict access to the plain
listener to the ingress, and forward `/peer/v1/rooms/*/mcp` without rewriting
its path. Preserve `Authorization`, `Mcp-Session-Id` and `Mcp-Protocol-Version`.
No second port or runtime MCP environment variables are needed.

A relay that only forwards collaboration traffic has no app-serving plane and
cannot execute room actions. This endpoint does not add one implicitly.

## Credentials

This initial endpoint accepts **certified person-device sessions**. Obtain one
using the existing HTTP device handshake:

1. Send a postcard-encoded `Hello { cert, client_nonce }` to
   `POST /peer/v1/session`.
2. Verify the returned `SessionChallenge.ack` against the expected host device,
   its certificate and your nonce. Never accept an unverified host challenge.
3. Sign `auth::challenge_digest(server_nonce)` with the device key and send the
   postcard-encoded `SessionFinish { handshake, ack2 }` to
   `POST /peer/v1/session/finish`.
4. Decode the response with `tilo_relay_protocol::web::decode_grant` and hex-encode
   the returned 32-byte `SessionGrant.token`.

The production handshake is implemented in
[`HttpsLink::connect`](../crates/browser/tilo-browser-peer/src/carrier.rs).
It uses the device key, not the person's root private key. The device must
already be certified and admitted to the room; MCP never enrolls a device,
redeems an invitation or widens its grants.

Supply the hex token as `Authorization: Bearer <token>` on **every** MCP request.
Existing callers may also use `X-Tilo-Session: <token>`. An Authorization header,
when present, takes precedence; an invalid one cannot fall back to another
credential.

Tokens retain the existing session lifetime: at most 15 minutes, with an idle
cutoff of 45 seconds. MCP traffic keeps an open session active, but does not
extend its absolute expiry. Reauthenticate and initialize a new MCP session
when a credential expires. A static client configuration is therefore useful
for short sessions, not unattended permanent access. This slice does not add
OAuth, token refresh, or a long-lived API-key issuer.

Registered agent and bot devices, including withdrawn delegations, are refused
rather than promoted to their owner's person principal. Supporting delegation
credentials requires preserving those principals and their narrower grants.

## Connect a client

For a client that accepts HTTP URLs and bearer headers, configure the URL above
and the certified session token. For example, in Claude Code:

```sh
claude mcp add --transport http tilo-room \
  https://host.example/peer/v1/rooms/11111111-1111-4111-8111-111111111111/mcp \
  --header "Authorization: Bearer $TILO_SESSION_TOKEN"
```

For Cursor or a remote agent's HTTP MCP configuration:

```json
{
  "mcpServers": {
    "tilo-room": {
      "url": "https://host.example/peer/v1/rooms/11111111-1111-4111-8111-111111111111/mcp",
      "headers": { "Authorization": "Bearer <certified-session-token>" }
    }
  }
}
```

Claude Desktop can use a compatible HTTP MCP connector/bridge; configurations
that only support stdio or require OAuth do not directly accept this credential.
Keep tokens out of version control and logs.

A raw initialization request illustrates the wire contract:

```sh
curl -i "https://host.example/peer/v1/rooms/$ROOM/mcp" \
  -H "Authorization: Bearer $TILO_SESSION_TOKEN" \
  -H 'Content-Type: application/json' \
  -H 'Accept: application/json, text/event-stream' \
  --data '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"external-client","version":"1"}}}'
```

Retain the returned `Mcp-Session-Id`. Send it with subsequent POSTs, including
`notifications/initialized`, `tools/list`, `resources/list`, `resources/read`
and `tools/call`. The Tilo tools include `tilo_list_apps`, `tilo_query`,
`tilo_resolve`, `tilo_invoke`, `tilo_status` and `tilo_cancel`.
`DELETE` with both credential and MCP session id retires that execution;
`GET` returns 405 because there is no server-initiated SSE stream. Notifications
return 202. JSON bodies are capped at 1 MiB.

## Scope and permissions

Each MCP session has a server-issued execution identity. Its actions use the
certified person's principal and device, pinned to the room in the URL. Client
names and history labels are provenance, not authority. Another credential or
room cannot use its MCP session id, even for the same person. A newly initialized
session cannot take over the first session's invocation status or cancellation.
Ending a session does not cancel already-dispatched app work.

Every HTTP request checks current certificate validity, delegation registration
and freshly read room membership. Catalogue and invocation dispatch repeat that
check, including when one tool call makes several action-host requests. The
existing app-serving plane still decides which actions that principal may use;
an admitted room member does not automatically gain owner-only room actions.
In particular, `RoomActions` currently restricts ordinary catalogue and mutation
requests to the host's owner, while permission-checked view reads retain their
existing rules. No computer input, private-profile viewer, watch lease or media
reader is attached to this endpoint.

Hosted records have canonical `record:…` provider names. The adapter exposes
bounded, endpoint-local numeric `peer_id` aliases and translates them back before
calling the action plane. These are routing handles, not device identities or
permission grants. Browser view locators keep their canonical provider routes.

MCP sessions and cached credential/room endpoints are bounded by the web
listener's `max_sessions` limit (256 by default); an endpoint retains at most
4096 provider aliases. Expired credentials are removed from the cache on later
MCP traffic. Requests with an `Origin` must match an explicitly configured
`--allowed-origin`; a wildcard does not bypass this check. Clients without an
Origin, such as native CLI clients, still require a certified bearer session.
