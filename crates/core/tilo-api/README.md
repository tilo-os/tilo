# tilo-api

The Tilo API server: Tilo's API, `/v1`, JSON over HTTP
([`docs/developers/api.md`](../../../docs/developers/api.md), described by
[`docs/api/openapi.json`](../../../docs/api/openapi.json)), and the same calls
for agents over MCP at `/mcp`. Every `/v1` route is a call of the key's run
(`session::Session::call`), the very tool call an agent makes, answered typed
(`v1`): one implementation, two faces.

An agent connects with nothing configured but the server's URL, the streamable
HTTP server

```text
https://mcp.tilo.sh
```

and authorizes itself through the API server: protected-resource and
authorization-server metadata, dynamic client registration, PKCE and the
authorization-code grant (the MCP authorization profile of OAuth 2.1). The
server sends the person to their own Tilo on the web, which asks them once
(naming the agent, what it may reach and until when) and answers with a
sign-in proof from a device of theirs and a certificate for a key the API server
generated for this one grant. The grant reaches **all my workspaces** (and
making new ones), or **one workspace** when the agent was pointed at
`https://mcp.tilo.sh/w/<workspace>`. Each MCP session is one agent run
(`Claude Code #1`), which the API server serves itself: the agent surface `tilo mcp`
serves (`tilo_mcp::McpServer`), over the grant's own session with the
relay the person's workspaces live on. Grants are listed and revoked at
`https://mcp.tilo.sh/grants`.

## Authority

The API server holds no device of anyone and no access authority over anyone.
For each grant it generates a key, and the person's root certifies it as an
agent's grant (`tilo_user_identity::DeviceCapabilities::agent`): scoped to
all of the person's workspaces or one, expiring within 90 days (the API server asks
for 89), administering nothing. A key minted for one workspace is certified by its
parent grant's key. The API server acts only with these keys, each the device of
its own session with the server:

- The server admits a grant as its person's agent in exactly the workspaces its
  scope reaches where it redeemed its person's invitation, holds it to an
  agent's role (Editor at most); what it invites or administers its grants in the
  workspace decide, asking its person first by default
  (`tilo_workspace_service::workspace_access`). Its certificate's expiry and
  its person's revocations, a minted grant's issuer included, are checked at
  every handshake.
- A grant reaching all workspaces enters through the site's entry to its home
  workspace, as the person's page does, and asks the relay there for the
  person's own workspace (`workspace.create-own`); a minted grant's parent asks for
  its invitation, and the minted grant never enters the home workspace
  (`docs/architecture/workspace-api.md`).

Revoking a grant at `/grants` (or the agent revoking its refresh token, or
`account/keys/<key>#revoke`) ends its tokens and sessions at once, with every grant
it minted, and forgets their keys. The person can also revoke the grant's key
from their devices like any other device; the server then refuses it from its
own revocation list. Past its certificate's expiry a grant issues no token and
the agent asks again.

## What a deployment provides

The binary reads its configuration from the environment:

| Variable | What |
|---|---|
| `TILO_API_LISTEN` | `HOST:PORT` to listen on, plain HTTP behind a TLS-terminating proxy |
| `TILO_API_ORIGIN` | The API server's public origin as agents reach it (`https://mcp.tilo.sh`) |
| `TILO_API_WEB_ENTRY` | The origin of the person's Tilo on the web, which signs them in and certifies grants (`https://tilo.sh`); its `entry.json` names the entry to the site's home workspace on the server (`home.admission`) |
| `TILO_API_HOST` | The host the person's workspaces live on, as the host writes it in `web-host.json`: its certificate and where it is dialled, its web origin among them (one object, or a JSON array of them) |
| `TILO_API_STATE` | A private file for registered clients, grants (with each live grant's key) and token digests; on persistent storage, one writer |

What it reaches:

- **The site** over HTTPS: `GET <TILO_API_WEB_ENTRY>/entry.json`.
- **The server's iroh endpoint** over UDP (QUIC), directly or through the
  packet relay it names: one session per MCP session, as the
  grant's key. The session enters the site's home workspace through its entry
  (the workspace `entry.json`'s `home.admission` names) in its first message,
  and each invitation it redeems is an entry on another stream of the same
  connection.

The server must be a persistent host, which keeps workspaces of people's own
for anyone who asks, and run its workspaces' records and agents (the daemon's
`agents` build).

- **Sign-in.** `TILO_API_WEB_ENTRY` must serve `/sign-in`: given
  `audience`, `challenge`, `return`, `pending`, `client`, `grant_key`,
  `grant_scope` (`all` or a workspace id) and `grant_expires` (Unix ms), it
  asks the person, then posts `pending`, `proof` (a
  `tilo_user_identity::SignInProof` from the page's certified device) and
  `grant` (the root's agent-grant certificate for `grant_key`), each
  canonical encoding, base64url, as a form to `return`; or `pending` and
  `decision=deny`.

## Image

`os/containers/tilo-api` builds the binary into the image the
deployment runs, `registry.tilo.so/tilo/api` (`dev-<commit>` from main's
dispatch, signed releases from stable): uid/gid 65532, plain HTTP on 8080/tcp,
its state on the `/state` volume, and nothing else written, so the root
filesystem can be read-only. t compiles the API server (`api-binaries`,
tools/ci/publish/server-binaries.sh) and the image only packages it with the
pinned Fedora base and the frozen RPM snapshot the server and VM host images use
(tools/ci/publish/api-build.sh). Every variable above is the
deployment's; the image carries none of them. `GET /health` answers
`{"ok":true}`.
