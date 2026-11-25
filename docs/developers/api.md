# The Tilo API

Tilo's programmatic interface is a JSON API over HTTP, versioned under `/v1`. Everything in a
workspace is a resource with fields and actions, and the API names them by path, as people and
agents do: `pdf-viewer/application.pdf`, and its action `read-form-fields`. The account is a
resource too: its workspaces and its keys.

Three things serve it:

- **An API server** (`tilo-api`, `https://api.tilo.sh` for hosted Tilo), which you call with a
  key (`Authorization: Bearer tilo_sk_…`). It holds no authority and no workspace of its own: each
  key carries the grant the person approved — a device key of theirs, certified by their root (or
  by a key of theirs it was minted from) — so any API server accepts it. Every call is forwarded to
  the Tilo host that serves the workspace best right now, and that host decides it. When no host
  that can read the workspace is online, the call answers `503 unavailable` saying so.
- **Any Tilo host directly**, from the SDKs, as a device of the person with its own key. The
  `tilo` command calls the same actions through its computer's daemon instead, as the account
  that runs it. The SDKs link the same Rust client the API server runs (`tilo-api`'s
  library), which serves these same routes in process: no second implementation.
- **A Tilo desktop's computer, to its own person's programs there.** These routes are served as
  HTTP on `$XDG_RUNTIME_DIR/tilo-session/api.sock`, in the login's private directory. The
  computer's daemon serves them, with the same library, as the person by the device's key, which
  never leaves the daemon: so a call needs no `Authorization`. The desktop only takes each
  connection there, refuses one from another account or a sandbox, and hands the rest to the
  daemon over its link. `tilolang repl --session` reaches a development session this way.

MCP is the same calls for agents (`/mcp` on the API server, `tilo mcp` on a computer): its tools
answer the bodies below as `structuredContent` (see [MCP's typed answers](sdk-wire.md)). One tool
set serves every key: a key for all the person's workspaces names the workspace each call acts in
(`workspace`, an id or its name), and a path under `account` needs none.

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
           "next_steps": ["GET /v1/workspaces/…/resources?path=notes/Todo lists its actions"]}}
```

`code` is stable and typed; `message` is for people; `next_steps` and `retry_after_ms` appear
when there are any. The HTTP status follows the code:

| status | codes |
|---|---|
| 400 | `invalid_request`, `invalid_arguments`, `api_version_unsupported` |
| 401 | `invalid_token`, `key_revoked`, `key_expired` (with `revoked_at_ms`/`expired_at_ms`) |
| 403 | `not_permitted`, `not_offered`, `beyond_scope` |
| 404 | `not_found`, `unknown_action`, `unknown_resource` |
| 409 | `idempotency_conflict`, `surface_changed` |
| 422 | `failed`, `refused`, `branch_incomplete`, `action_confirmation_required` and every other code an app gives |
| 503 | `unavailable` |

## Idempotency

Every mutating call (`POST`, `PUT`, `DELETE`) takes an `Idempotency-Key` header (1–255
printable characters; the SDKs always send one). The same key with the same request answers the
first answer again (`Idempotent-Replayed: true`) and never runs anything twice; the same key with
another request is `409 idempotency_conflict`. Asked again while the first is still running,
the same key and request answer `503 unavailable` with a `Retry-After`; the SDKs ask again and
get the first answer once it lands. An action's invocation is derived from the key,
so the host itself never runs it twice, also across API server restarts. A call still pending
(waiting for a person) is not stored: retrying it with its key asks about the same invocation
again, which is how a client awaits it. Keys are remembered for 24 hours per key holder.

## Invocations and attribution

Every call is an invocation, which History, activity and presence name. A call names its group
with `Tilo-Invocation: <id>` and its title with `Tilo-Invocation-Title: <words>` — on every call
of the group, since each step's row is named by the title of the call that made it; every call
carrying the same id is one invocation, one undo step per app
(`POST …/invocations/{invocation}/undo` takes it back). Without `Tilo-Invocation` each call is
an invocation of its own, named for the key ("Visa backend #3").

## Account

| | |
|---|---|
| `GET /v1` | `{"api_version": "1", "server": "…", "key": {…}}`: the API and, with a key, what it reaches |
| `GET /v1/resources?path=` | the account's resources and actions, as a workspace's are listed |
| `GET /v1/resources/account/usage` | its `fields`: the person's plan and per workspace what it keeps and the VM time it ran |
| `POST /v1/resources/{path}/actions/{action}` | runs an action of the account, as a workspace's action is run: `{"status", "path", "result"}` |

A workspace's export is the walk of its listings, each resource read whole: the SDKs'
`workspace.export()` makes it.

The account's actions, each run with `POST /v1/resources/account/{resource}/actions/{action}`
(MCP: `tilo_do account/{resource}#{action}`), answer their `result`:

| | |
|---|---|
| `workspaces#list` `{labels?}` | `{"workspaces": [{"id", "name", "host", "role", "labels", "unattended"}]}`: every workspace the key reaches, wherever it is hosted; with `labels`, those labelled with every one given. A key for one workspace lists that one |
| `workspaces#create` `{name, labels?}` | `{"workspace"}`, the one made, its `role` `owner`; the same `Idempotency-Key` answers the one it made |
| `workspaces#branch` `{workspace, name, labels?}` | `{"workspace"}`, a branch of a workspace of the person's own: a new one of theirs holding every resource of it with its history and the files they name, without its conversations, people's drafts and places, or anyone but the person. Answered once it holds everything; one that could not take a part is deleted again and the call answers `422 branch_incomplete` naming the part. The same `Idempotency-Key` answers the branch it made |
| `workspaces#rename` `{workspace, name}` | `{"workspace"}`, called that by every list and device of it; its owners and moderators name it |
| `workspaces#unattended` `{workspace, on}` | lets the keys made for this workspace alone run commands in its Linux machine (`terminal#run`) without asking the person each time, here and in branches made from it. `on: true` asks the person on their approving device: `202` pending with an `ask`, the run answering `{"workspace", "unattended": true}` once they allow it and failing if they do not; `on: false` answers at once and asks again from the next command. Run it in the workspace (`POST /v1/workspaces/{workspace}/resources/account/workspaces/actions/unattended`) to follow the pending run there. Only the workspace's owner has it; a key reaching all their workspaces is still asked. Listings say `"unattended": true` while it is on |
| `workspaces#delete` `{workspace}` | `{"deleted", "revoked_keys": [...]}`: final, with everything in it and every key minted for it |
| `GET /v1/workspaces/{workspace}/resources?path=record:tilo.grants` | the keys the workspace admitted, each a record of its own (no action lists keys): one row per grant this key may read, `record:tilo.grants:<grant>`, and `GET /v1/workspaces/{workspace}/resources/record:tilo.grants:<grant>` reads it, `{"fields": {"id", "person", "device", "name", "scope", "expires_ms", "parent", "created_ms", "revoked_ms"}}`. A key reads its own grant and the ones it minted, a device of the person every grant of theirs, and whoever the workspace lets revoke grants every one |
| `keys#mint` `{workspace, name?, expires_in?}` | a key for one workspace, minted from this one: `{"grant", "name", "token", "workspace", "mcp_url", "expires_at_ms"}`; the person is not asked |
| `keys/<key>#revoke` (a key by its name or grant) | revokes this key or one it minted, and every key it minted; the workspace a revoked key reaches records the revocation, so every API server and holder refuses it |

A key for one workspace lists the workspaces and keys it reaches and revokes its keys; every other
action of the account answers `not_permitted`.

## Resources

| | |
|---|---|
| `GET /v1/workspaces/{workspace}/resources?path=&cursor=&limit=&kind=` | what is under `path` (the apps without one): `{"path", "kind", "rows": [...], "actions": [...], "kinds": {...}, "next_cursor", "text"}` |
| `GET /v1/workspaces/{workspace}/resources/{path}?cursor=` | one resource: `{"path", "kind", "fields": {...}, "children": [...], "cursor", "text"}` |
| `POST /v1/workspaces/{workspace}/resources/search/actions/find` | the workspace's search, `{"query", "app", "limit"}`: its `result` is `{"hits": [{"path", "kind", "label", "action"}], "query"}`, best first |
| `GET /v1/workspaces/{workspace}/views?path=&picture=&scale=` | what the app's window shows for `path`: `{"path", "app", "shown", "text", "tree", "nodes", "viewport", "window_origin", "revision"}`, and `picture` (a `tilo.content:` of a PNG) with `picture=true` |

A row of `rows` is `{"path", "name", "label", "kind", "has_children", "reads", "actions"}`, its
`actions` the ids of the actions it offers, and `reads` whether a read answers it as a resource of
its own: a row that is only a place inside one (an option, a choice) says no, and holds nothing of
its own to read. `kinds` describes them once for each kind of row on the page
(`{"notes.Page": [...]}`), so a page of a thousand notes does not repeat the same action rows a
thousand times. An action row is `{"id", "label", "params": [{"name", "ty", "required",
"description", "choices", "schema"}], "kind": "read"|"undoable"|"outside"}`. `next_cursor` is
present while more rows follow; passed back as `cursor` it answers the next page.

A read of a resource answers its lists a page at a time: 25 entries of each list by default,
`pageSize` up to 200. A page that leaves entries out says how many each list holds (`totals`) and
answers `cursor` for the next page.

## Actions

`POST /v1/workspaces/{workspace}/resources/{path}/actions/{action}` runs `action` on the resource,
with its arguments as the body (`{}` for none). Headers: `Idempotency-Key`, `Tilo-Invocation`,
`Prefer: wait=<seconds>` (how long to wait for the answer, default 10, at most 60), and
`Tilo-Surface: <app>@<digest>` from generated types (see [Types](#types-and-versions)).

- **`200`** it ran:
  `{"status": "ok", "invocation": "…", "path": "notes/Todo#append-paragraph", "result": {...},
  "resources": [...], "link": "…", "edited_by_person": {...}}`
- **`202`** it has not finished, or it waits for the person:
  `{"status": "pending", "invocation": "…", "ask": {...}}` with
  `Location: /v1/workspaces/{workspace}/invocations/{invocation}`.
  Nothing ran yet when `ask` is there: the person is being asked on their device.
- **`422`** the app refused or it failed: the error shape, with the action's row as `action`.

Every resource also publishes `group`: `{"actions": [{"action": "create", "args": {...}}, …]}`
applies those actions of the resource in order as one all-or-nothing step — one transaction,
one undo, and History records each member, attributed to the caller. If any member refuses,
nothing applies. A group answers no `result`: its members' values are not carried back.

## Invocations

`{invocation}` is an id from an action's answer, or a listed invocation's `id`.

| | |
|---|---|
| `GET /v1/workspaces/{workspace}/resources/record:tilo.invocations:` | the workspace's invocations in records the key may read, newest first, whoever made them, read whole through the record API (MCP: `tilo_read record:tilo.invocations:`; `tilo invocations -w <workspace>` pages it). Its `fields` are `{"items": [...], "total", "waiting"}`, every invocation; a reader pages them itself, 50 unless it says (at most 200). One invocation by its id is `record:tilo.invocations:<id>`. An invocation is `{"id", "calls", "origin": {"kind", "id"}, "retries", "spend": {"provider", "model", "input_tokens", "output_tokens", "cost_micros"}, "step", "by": {"name", "kind"}, "title", "status": "waiting"|"running"|"ended"|"failed"|"cancelled"|"undone", "failure", "started_ms", "ended_ms", "action", "summary", "changes", "undone", "resources", "undoable", "undo_blocked", "recorded", "remembered"}`. `origin` is the resource the call was made from (its `$from`, else the conversation its turn belongs to); `retries` names the invocation a retry runs again; `spend` is what a model call it made cost. One that changed a resource is listed after the host restarts, from that resource's history, without an `id` (`remembered: false`); `step` names it either way. Of each the key is told only what lies in records it may read now; one with nothing there is not listed, and the totals count only what is listed. The record's own actions stop it (`#cancel`) and run it again (`#retry`, as its caller, under a new id), which its caller, the person its agent works for, or anyone who may write its `origin` may do |
| `GET /v1/workspaces/{workspace}/invocations/{invocation}?wait=` | the invocation as the action answers it; `wait` (seconds, at most 60) holds until it is no longer pending |
| `POST /v1/workspaces/{workspace}/invocations/{invocation}/cancel` | stops it: the invocation, now `cancelled` |
| `POST /v1/workspaces/{workspace}/invocations/{invocation}/undo` | takes back what it changed: `{"status": "ok", "undone": [...]}` |
| `GET /v1/workspaces/{workspace}/invocations/{invocation}/output` | its events as a stream (below), ending when it ends |

## Events

`GET /v1/workspaces/{workspace}/events?path=` is a server-sent event stream (`text/event-stream`).
Each event's `id` is a cursor: reconnecting with `Last-Event-ID` resumes after it, so each event is
seen once. `event` is the kind, `data` its JSON:

- `change` `{"path", "resources", "change", "sentence", "actor", "invocation", "title", "group", "group_kind", "time_ms", "undone"}`
  — `resources` names every resource the change wrote: each entry's `path`, the `app` it belongs
  to, the id that app gives it, and the `item` where the change names one. `actor` is structured
  (`{"kind": "person"|"agent", "name": …, …}`), `invocation` is the invocation that made it,
  `title` the name of the group it is a step of and `group` that group. A change a person made by
  hand has no `group`.
- `invocation` `{"phase": "started"|"succeeded"|"failed"|"cancelled", "invocation", "path", "title", "group", "time_ms"}`
- `presence` `{"here": [{"who", "name", "kind": "person"|"agent"}]}`

A comment line (`: keep-alive`) every 15 seconds keeps idle connections open.

## The person's model

`POST /v1/workspaces/{workspace}/model/ask` asks the person's own model one turn: the model they
chose on the server that keeps their workspace, asked there with the key their vault keeps, as a
page asks it. The body is the conversation, `{"messages", "tools", "tool_choice",
"max_output_tokens"}` (each message `{"role", "content"}`). Only the person on a device of theirs
is answered; an agent's key is not.

The answer is a server-sent event stream that follows the model as it writes:

- `text` `{"text"}` and `reasoning` `{"text"}` — what the model wrote since the event before;
- `answer` — the whole turn, ending the stream: `{"Ok": {"message", "stop_reason", "usage"}}`,
  or `{"Err": {"kind", "message"}}` when the model gave none (`no_provider`: none is set up yet;
  `authentication`: no key is kept for it);
- `error` — the API's refusal shape, ending the stream when the server stopped answering.

A computer's own apps ask their model this way, through the session `tilo login` holds.

## Webhooks

On the API server, a workspace's events can be posted to a URL instead:

| | |
|---|---|
| `account/webhooks#create` `{workspace, url, events?, path?}` | `{"id", "workspace", "url", "events", "secret"}`; `secret` is shown once |
| `account/webhooks#list` `{workspace?}` | the key's webhooks, without their secrets |
| `account/webhooks#delete` `{webhook}` | removes one |

Each is an action of the account, run as any other: `POST /v1/resources/account/webhooks/actions/create`.

Each delivery is `POST url` with the event as the body (`{"id", "type", "workspace", "data"}`),
`Tilo-Webhook-Id`, `Tilo-Delivery` (unique per event) and `Tilo-Signature: t=<unix seconds>,
v1=<hex HMAC-SHA256 of "<t>.<body>" under the secret>`. A `2xx` answer delivers it; anything else
or no answer within 10 seconds is retried with growing waits (5 s to 1 h, 8 attempts), in order.
Check the signature and that `t` is recent; deduplicate by `Tilo-Delivery`.

## Files

A file's bytes go into the workspace at their own address: the lowercase BLAKE3 hash of the
bytes, 64 hex characters. The address is what makes a file go up once, whatever a caller would
have called it, and what a client says to ask whether the workspace already has it. A put of
bytes it already holds is answered as complete without storing them twice.

| | |
|---|---|
| `PUT /v1/workspaces/{workspace}/contents/{hash}` (the file's bytes) | `201` `{"content": "tilo.content:<reference>", "offset": <size>, "size"}`, what an action takes: the whole file in one request, or one piece of it |
| `GET /v1/workspaces/{workspace}/contents/{hash}` | the bytes a reference names, with `Range` |

A put's `{hash}` is the BLAKE3 hash of the file's bytes, lowercase hex: the same bytes are the
same address, so a file goes up once, and a put of bytes the server has already kept answers the
same reference without storing them again. The answer's `content` is the reference the workspace
keeps them by; in a workspace whose content is sealed it carries the key that opens it after a
`.`, and a `GET` takes that reference whole.

A file larger than one request should carry goes up in pieces, each request carrying one. Its
`Content-Range: bytes {from}-{to}/{size}` says where the piece starts and ends and how big the
whole file is, and the first piece opens the put. One request carries at most 64 MiB.

A piece sent from an offset the put is not at answers `409 offset_mismatch`, naming where it
stands. A piece sent again after an answer that did not arrive costs nothing: the answer says
where the put stands now (`200` `{"offset", "size"}`). The piece that completes the file
answers `201` `{"content", "offset", "size"}`, and a put of content whose every byte is
already there answers `409 upload_complete`. A put nobody continues is dropped after six
hours. `tilo files put` sends a large file this way, and resumes it after a connection that
failed.

Whatever a file ends in is `tilo.content:<hash>`, which an action takes the file by. Nothing
names an app yet — a file's **bytes** say which app opens it. Opening the file there is that
app's `open` with the file: `POST …/resources/pdf-viewer/actions/open`
`{"content": "tilo.content:<hash>", "name": "forms/visa.pdf"}`. PDF Viewer, Writer, Sheets and
Slides open each file in a resource of its own, so a folder of PDFs is a PDF each; Notes and
Photos take it into the notebook or library the workspace keeps. The answer's
`result.provider` names the resource, where a person opens it and search finds it. An app that
does not open the file's type refuses it and makes no resource.

A folder goes into the workspace's own folder in one action: `POST …/resources/files/actions/open`
with `files`, a list of `content`, `name` and `dir`, each file's bytes uploaded first. A large
list goes up as content of its own and the action names its `tilo.content:` reference instead,
so the action stays one bounded request however many files the folder holds (up to 10,000).
They are written as one change of the workspace's tree, so the folder costs its chunks rather
than its files, and History has one entry to take back whole. `tilo files put` does this for a
folder; a folder past 10,000 goes in as several lists.

## People and programs

| | |
|---|---|
| `workspace/members` `#list`, `#add {person, role, name?}`, `#role {person, role}`, `#remove {person}`, `#name {name}` | the people in it and their roles; holding a person by their user public key at a role at once, moving them, removing them and every device of theirs, and the name you go by there |
| `workspace/invitations` `#invite {role, invitee_name?, wait_list_entry?}`, `#list`, `#cancel {invite}` | an invitation's `link`, which signs the person in and lands them there; the invitations still open, and taking one back |
| `workspace/waiting` `#list`, `#unlist {entry}` | the addresses left at the workspace's entry while it takes people by invitation |
| `POST /v1/workspaces/{workspace}/programs` `{program, confirm?, session?}` | runs a tilolang program as one step: `{"status": "ok"|"preview"|"failed", "output", "changed", "steps", "text", "session"}` |

A program runs in a program session: the `let`s its programs bound, the references its answers
showed (`n3`, in `text`), and the steps `undo` and `history task` take back and list. An answer
names its `session`; send it with the next program to continue there. A program without one starts
a new session. A key keeps its 16 most recently used sessions; a program naming one that ended
answers `404 not_found`. A program that would change much or destroy something answers `preview`
and changes nothing until it is sent again with `confirm: true`. `tilolang repl` is a program
session typed by hand.

## Types and versions

Each app publishes its compiled surface (`surface.json`, compiled from the author's `surface.toml`), and its digest is the surface's version
(`<app>@<first 16 hex of its SHA-256>`). Generated types name it: a call made through one sends
`Tilo-Surface`, and a workspace whose app publishes another surface answers
`409 surface_changed`, naming both, instead of running it: `tilo sdk gen` writes the types again.
Untyped calls (`ws.do(path, **args)`) send none.
