# tilo-front-door

The MCP front door of hosted Tilo. An agent connects with one line, for example

```sh
claude mcp add --transport http tilo https://mcp.tilo.sh
```

and authorizes itself through the door: protected-resource and
authorization-server metadata, dynamic client registration, PKCE and the
authorization-code grant (the MCP authorization profile of OAuth 2.1). The
door sends the person to their own Tilo on the web, which asks them once
(naming the agent, what it may reach and until when) and answers with a
sign-in proof from a device of theirs and a certificate for a key the door
generated for this one grant. The grant reaches **all my workspaces** (and
making new ones), or **one workspace** when the agent was pointed at
`https://mcp.tilo.sh/w/<workspace>`. Each MCP session is one agent run
(`Claude Code #1`), carried to the person's runtime. Grants are listed and
revoked at `https://mcp.tilo.sh/grants`.

## Authority

The door holds no device of anyone and no standing authority over anyone.
For each grant it generates a key, and the person's root certifies it as an
agent's grant (`tilo_user_identity::DeviceCapabilities::agent`): scoped to
all of the person's workspaces or one, expiring within 90 days (the door asks for 89), governing
nothing. The door acts only with that key:

- on the person's **hosts** (`room.list`, `room.create`,
  `identity.revocations`), which treat an agent's grant as their person
  within its scope: it makes workspaces only when it reaches all of them,
  lists only what it reaches, and governs, enters or administers nothing;
- at the person's **runtime**, which challenges every session it opens and
  checks the grant's proof, its scope and the person's revocations itself.

Revoking a grant at `/grants` (or the agent revoking its refresh token) ends
its tokens and sessions at once and forgets its key. The person can also
revoke the grant's key from their devices like any other device; hosts and
the runtime then refuse it from their own revocation lists. Past its
certificate's expiry a grant issues no token and the agent asks again.

## What a deployment provides

The binary reads its configuration from the environment:

| Variable | What |
|---|---|
| `TILO_DOOR_LISTEN` | `HOST:PORT` to listen on, plain HTTP behind a TLS-terminating proxy |
| `TILO_DOOR_ISSUER` | The door's public origin as agents reach it (`https://mcp.tilo.sh`) |
| `TILO_DOOR_WEB_ENTRY` | The origin of the person's Tilo on the web, which signs them in and certifies grants (`https://tilo.sh`) |
| `TILO_DOOR_STATE` | A private file for registered clients, grants (with each live grant's key) and token digests; on persistent storage, one writer |
| `TILO_DOOR_HOSTS` | The hosts registry below |

The hosts registry is JSON the hosting side writes and updates; the door
reads it again when it changes. It names places, not authority:

```json
{"people": {"<identity root key, hex>": {
  "hosts": [{
    "name": "<host name>",
    "default": true,
    "device": "<the host's device key, hex>",
    "addr": <the host's iroh EndpointAddr, as serde writes it>,
    "agents": "http://<internal address of the person's runtime>",
    "agents_token_file": "…"
  }]
}}}
```

- **The person's runtime.** One `tilo-runtime` per person (beside their
  host) serves every workspace it takes part in behind one HTTP endpoint at
  `agents`: `/w/<workspace>/mcp` and `/w/<workspace>/join`, behind a bearer
  token the door holds (`agents_token_file`), with grant proofs required
  (`TILO_RUNTIME_MCP_HTTP_GRANTS=required`, so the token alone reaches
  nothing). A workspace `create_workspace` makes is joined there before the
  tool answers, so the next call acts in it.
- **Revocations.** The door asks the person's hosts for the revocations they
  enforce (`identity.revocations`), the same list their handshakes read,
  which the person's devices keep current; nothing is mirrored to the door.
- **Sign-in.** `TILO_DOOR_WEB_ENTRY` must serve `/sign-in`: given
  `audience`, `challenge`, `return`, `pending`, `client`, `grant_key`,
  `grant_scope` (`all` or a workspace id) and `grant_expires` (Unix ms), it
  asks the person, then posts `pending`, `proof` (a
  `tilo_user_identity::SignInProof` from the page's certified device) and
  `grant` (the root's agent-grant certificate for `grant_key`), each
  canonical encoding, base64url, as a form to `return`; or `pending` and
  `decision=deny`.
