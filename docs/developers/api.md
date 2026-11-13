# The Tilo API

Tilo's programmatic interface is a JSON API over HTTP, versioned under `/v1`. Everything in a
workspace is a resource with fields and actions, and the API names them by path, as people and
agents do: `pdf-viewer/application.pdf`, and its action `read-form-fields`. The account is a
resource too: its workspaces and its keys.

Two things serve it:

- **An API server** (`tilo-api`, `https://api.tilo.sh` for hosted Tilo), which you call with a
  key (`Authorization: Bearer tilo_sk_…`). It holds no authority of its own: each key is a device
  key of the person, certified by their root (or by a key of theirs it was minted from), and every
  call goes to the Tilo host that keeps the workspace, which decides it.
- **Any Tilo host directly**, from the SDKs and the `tilo` command, as a device of the person
  with its own key. The SDKs link the same Rust client the API server runs (`tilo-api`'s
  library), which serves these same routes in process: no second implementation, no socket.

MCP is the same calls for agents (`/mcp` on the API server, `tilo mcp` on a computer): its tools
answer the bodies below as `structuredContent` (see [MCP's typed answers](sdk-wire.md)).

The OpenAPI 3.1 description is generated from the same catalogue and types the calls use:
[`docs/api/openapi.json`](../api/openapi.json) (`tilo-api openapi --check` fails when it drifts).

## Conventions

- **Paths.** `{workspace}` is a workspace id (a UUID). `{path}` is a resource path; its segments
  are percent-encoded one by one (`notes/Q3%2F4%20plan`), so a resource named `actions` or
  holding `/` is still one segment.
- **Bodies** are JSON (`Content-Type: application/json`), but for uploads.
- **Times** are Unix milliseconds (`*_ms`).
- **Versions.** Every answer carries `Tilo-Api-Version: 1`. `GET /v1` says the API version and
  the server's build; a client made for another major version is refused with
  `api_version_unsupported`.

## Errors

Every refusal has one shape, whatever route it came from:

```json
{"error": {"code": "invalid_arguments", "message": "…",
           "next_steps": ["GET /v1/workspaces/…/resources?path=notes/Todo lists its actions"],
           "retry_after_ms": 2000}}
```

`code` is stable and typed; `message` is for people; `next_steps` and `retry_after_ms` appear
when there are any. The HTTP status follows the code:

| status | codes |
|---|---|
| 400 | `invalid_request`, `invalid_arguments`, `api_version_unsupported` |
| 401 | `invalid_token`, `key_revoked`, `key_expired` (with `revoked_at_ms`/`expired_at_ms`) |
| 403 | `not_permitted`, `beyond_scope` |
| 404 | `not_found`, `unknown_action`, `unknown_resource` |
| 409 | `idempotency_conflict`, `surface_changed` |
| 422 | `failed`, `refused`, `action_confirmation_required` and every other code an app gives |
| 429 | `rate_limited` |
| 503 | `unavailable` |

## Idempotency

Every mutating call (`POST`, `PUT`, `DELETE`) takes an `Idempotency-Key` header (1–255
printable characters; the SDKs always send one). The same key with the same request answers the
first answer again (`Idempotent-Replayed: true`) and never runs anything twice; the same key with
another request is `409 idempotency_conflict`. An action's invocation is derived from the key,
so the host itself never runs it twice, also across API server restarts. A call still pending
(waiting for a person) is not stored: retrying it with its key asks about the same invocation
again, which is how a client awaits it. Keys are remembered for 24 hours per key holder.

## Runs and attribution

Every call is a step of a run, which History, activity and presence name. A call names its run
with `Tilo-Run: <id>` and its title with `Tilo-Run-Title: <words>` — on every call of the run,
since each step's row is named by the title of the call that made it; every call carrying the
same id is one run, one undo step per app (`POST …/runs/{run}/undo` takes it back). Without
`Tilo-Run` each call is a run of its own, named for the key ("Visa backend #3").

## Account

| | |
|---|---|
| `GET /v1` | `{"api_version": "1", "server": "…", "key": {…}}`: the API and, with a key, what it reaches |
| `GET /v1/account` | the person's plan and per workspace what it keeps and the VM time it ran |
| `GET /v1/workspaces` | `{"workspaces": [{"id", "name", "host", "role"}]}`: every workspace the key reaches, wherever it is hosted |
| `POST /v1/workspaces` `{name}` | `201` the workspace made: `{"id", "name", "host", "role": "owner"}` |
| `DELETE /v1/workspaces/{workspace}` | `{"deleted": "…", "revoked_keys": [...]}`: final, with everything in it and every key minted for it |
| `GET /v1/keys` | `{"keys": [{"grant", "name", "scope", "expires_at_ms", "last_used_at_ms", "created_by"}]}` |
| `POST /v1/keys` `{workspace, name?, expires_in?}` | `201` a key for one workspace, minted from this one: `{"grant", "token", "mcp_url", "expires_at_ms"}`; the person is not asked |
| `DELETE /v1/keys/{grant}` | revokes this key or one it minted, and every key it minted |

## Resources

| | |
|---|---|
| `GET /v1/workspaces/{workspace}/resources?path=&cursor=&limit=&kind=` | what is under `path` (the apps without one): `{"path", "kind", "rows": [...], "actions": [...], "next_cursor"}` |
| `GET /v1/workspaces/{workspace}/resources/{path}?cursor=` | one resource: `{"path", "kind", "fields": {...}, "children": [...], "cursor"}` |
| `GET /v1/workspaces/{workspace}/search?q=&limit=` | `{"hits": [{"path", "kind", "label", "action"}], "query"}`, best first |
| `GET /v1/workspaces/{workspace}/links?path=&mode=` | `{"path", "link"}`: the link a person opens it with (invite them first) |

A row of `rows` is `{"path", "name", "label", "kind", "has_children", "actions"}`; an action row
is `{"id", "label", "params": [{"name", "ty", "required", "description", "choices", "schema"}],
"kind": "read"|"undoable"|"outside"}`.

## Actions

`POST /v1/workspaces/{workspace}/resources/{path}/actions/{action}` runs `action` on the resource,
with its arguments as the body (`{}` for none). Headers: `Idempotency-Key`, `Tilo-Run`,
`Prefer: wait=<seconds>` (how long to wait for the answer, default 10, at most 60), and
`Tilo-Surface: <app>@<digest>` from generated types (see [Types](#types-and-versions)).

- **`200`** it ran:
  `{"status": "ok", "run": "…", "path": "notes/Todo#append-paragraph", "result": {...},
  "resources": [...], "link": "…", "edited_by_person": {...}}`
- **`202`** it has not finished, or it waits for the person:
  `{"status": "pending", "run": "…", "ask": {...}}` with `Location: /v1/workspaces/{workspace}/runs/{run}`.
  Nothing ran yet when `ask` is there: the person is being asked on their device.
- **`422`** the app refused or it failed: the error shape, with the action's row as `action`.

## Runs

`{run}` is an invocation id from an action's answer.

| | |
|---|---|
| `GET /v1/workspaces/{workspace}/runs/{run}?wait=` | the run as the action answers it; `wait` (seconds, at most 60) holds until it is no longer pending |
| `POST /v1/workspaces/{workspace}/runs/{run}/cancel` | stops it: the run, now `cancelled` |
| `POST /v1/workspaces/{workspace}/runs/{run}/undo` | takes back what it changed: `{"status": "ok", "undone": [...]}` |
| `GET /v1/workspaces/{workspace}/runs/{run}/output` | its events as a stream (below), ending when it ends |

## Events

`GET /v1/workspaces/{workspace}/events?path=` is a server-sent event stream (`text/event-stream`).
Each event's `id` is a cursor: reconnecting with `Last-Event-ID` resumes after it, so each event is
seen once. `event` is the kind, `data` its JSON:

- `change` `{"path", "change", "sentence", "by", "run", "time_ms", "undone"}`
- `run` `{"phase": "started"|"succeeded"|"failed"|"cancelled", "run", "path", "title", "time_ms"}`
- `presence` `{"here": [{"who", "name", "kind": "person"|"agent"}]}`

A comment line (`: keep-alive`) every 15 seconds keeps idle connections open.

## Webhooks

On the API server, a workspace's events can be posted to a URL instead:

| | |
|---|---|
| `POST /v1/workspaces/{workspace}/webhooks` `{url, events?, path?}` | `201` `{"id", "url", "events", "secret"}`; `secret` is shown once |
| `GET /v1/workspaces/{workspace}/webhooks` | the webhooks, without their secrets |
| `DELETE /v1/workspaces/{workspace}/webhooks/{id}` | removes one |

Each delivery is `POST url` with the event as the body (`{"id", "type", "workspace", "data"}`),
`Tilo-Webhook-Id`, `Tilo-Delivery` (unique per event) and `Tilo-Signature: t=<unix seconds>,
v1=<hex HMAC-SHA256 of "<t>.<body>" under the secret>`. A `2xx` answer delivers it; anything else
or no answer within 10 seconds is retried with growing waits (5 s to 1 h, 8 attempts), in order.
Check the signature and that `t` is recent; deduplicate by `Tilo-Delivery`.

## Files

| | |
|---|---|
| `POST /v1/workspaces/{workspace}/uploads` (the file's bytes, its `Content-Type`) | `201` `{"content": "tilo.content:<hash>", "size"}`: what an action takes |
| `GET /v1/workspaces/{workspace}/content/{content}` | the bytes an action made (`tilo.content:<hash>`), with `Range` |

## People and programs

| | |
|---|---|
| `GET /v1/workspaces/{workspace}/members` | the people in it, their roles, and the invitations still open |
| `POST /v1/workspaces/{workspace}/members/invitations` `{role, email?}` | `201` `{"link"}` that signs them in and lands them there |
| `POST /v1/workspaces/{workspace}/programs` `{program, confirm?}` | runs a tilolang program as one step: `{"status": "ok"|"preview"|"failed", "output", "changed", "steps"}` |

## Types and versions

Each app publishes its surface (`surface.toml`), and its digest is the surface's version
(`<app>@<first 16 hex of its SHA-256>`). Generated types name it: a call made through one sends
`Tilo-Surface`, and a workspace whose app publishes another surface answers
`409 surface_changed`, naming both, instead of running it: `tilo sdk gen` writes the types again.
Untyped calls (`ws.do(path, **args)`) send none.
