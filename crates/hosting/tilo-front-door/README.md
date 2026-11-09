# tilo-front-door

The MCP front door of hosted Tilo. An agent connects with one line, for example

```sh
claude mcp add --transport http tilo https://<door>
```

and authorizes itself through the door: protected-resource and
authorization-server metadata, dynamic client registration, PKCE and the
authorization-code grant (the MCP authorization profile of OAuth 2.1). The
person signs in with a device of theirs and chooses **all my workspaces** or
**one workspace**; `https://<door>/w/<workspace>` asks for that one directly.
Each MCP session is one agent run (`Claude Code #1`), carried to the
`tilo-runtime` HTTP MCP endpoint of the workspace's host. Grants are listed and
revoked at `https://<door>/grants`.

## What a deployment provides

The binary reads its configuration from the environment:

| Variable | What |
|---|---|
| `TILO_DOOR_LISTEN` | `HOST:PORT` to listen on, plain HTTP behind a TLS-terminating proxy |
| `TILO_DOOR_ISSUER` | The door's public origin (`https://…`), as agents reach it |
| `TILO_DOOR_WEB_ENTRY` | The origin of the person's Tilo on the web, which signs them in |
| `TILO_DOOR_STATE` | A private file for registered clients, grants and token digests; on persistent storage, one writer |
| `TILO_DOOR_HOSTS` | The hosts registry below |

The hosts registry is JSON the hosting side writes and updates; the door reads
it again when it changes:

```json
{"people": {"<identity root key, hex>": {
  "device": {"key_file": "…", "cert_file": "…"},
  "revocations_file": "…",
  "hosts": [{
    "name": "<host name>",
    "default": true,
    "device": "<the host's device key, hex>",
    "addr": <the host's iroh EndpointAddr, as serde writes it>,
    "agents": "http://<internal address>/w/{workspace}/mcp",
    "agents_token_file": "…"
  }]
}}}
```

- **The door's device of each person.** The door asks a person's hosts to list
  and create workspaces (`room.list`, `room.create`) as a device of that
  person, so each person served needs a device key whose certificate their
  identity root signed (canonical encoding). It is issued when the person's
  host is installed, beside the node and session certificates.
- **Per-workspace agent endpoints.** For every workspace a host serves, a
  `tilo-runtime` HTTP MCP endpoint must answer at `agents` with `{workspace}`
  replaced, behind a bearer token the door holds (`agents_token_file`). A
  workspace the door just created must get one as soon as it exists.
- **Revocations.** `revocations_file` mirrors the person's revocation list
  (canonical encoding of the entries) so a revoked device signs nobody in.
- **Sign-in.** `TILO_DOOR_WEB_ENTRY` must serve `/sign-in`: given `audience`,
  `challenge`, `return` and `pending`, it signs the challenge with the page's
  certified device key (`tilo_user_identity::SignInProof`) once the person
  agrees, and posts `pending` and `proof` (canonical encoding, base64url) as a
  form to `return`.
