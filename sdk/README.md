# Tilo SDKs

Script a Tilo workspace from your own code: the same resources, actions,
permissions, history and undo a person clicking or an agent over MCP gets.
There is no second API. The design is `docs/developers/sdk.md`.

- `python/`: the `tilo-sdk` package (`AsyncTilo` and the sync `Tilo`), imported as `tilo`.
  uv project, Python 3.11 or newer, `httpx` for HTTP.
- `typescript/`: the `tilo-sdk` package, Promise-only. bun project, ESM, `fetch`.

```sh
uv add tilo-sdk          # Python:        from tilo import Tilo
bun add tilo-sdk         # TypeScript:    import { Tilo } from "tilo-sdk"
```

Both speak Tilo's API, `/v1` (`docs/developers/api.md`): JSON over HTTP, the
calls an agent makes, answered typed. One transport sends them to an API
server; another serves the same routes in process against a Tilo host, through
the Rust client the API server itself runs (`AsyncTilo.host` / `Tilo.host`), so
there is no second implementation and no socket.

Both answer the same surface, `snake_case` in Python and `camelCase` in
TypeScript: `workspaces.list/create/delete`, `keys.list/create/mint/revoke`,
`ws.ls/read/find/do/link/program/status/watch`, `ws.files.upload`,
`ws.keys.mint`, `ws.run(title)` for one undo step per app, `ws.resource(path)`
as a typed object over a read and `field-edit`, `res.many(action, items)` for several
actions as one step per call, `Refused`, `AskPending` and
`ProtocolError`, `SurfaceChanged` when an app's surface moved past the
generated types, and for a key the API turns away `KeyRevoked`, `KeyExpired`
or `KeyRefused`.

A key is `TILO_API_KEY` and the API server is `TILO_API_URL`
(`https://api.tilo.sh` by default). `tilo sdk gen` writes per-app types into
`python/src/tilo/types/` and `typescript/src/types/`; both ship the proxies
those types stand on.

A record action that changes it and answers nothing also has a bulk form. Generated types
expose it as `create_many` (Python) and `createMany` (TypeScript), over a typed `CreateArgs`
list; `res.many("create", items)` is the same without generated types. The SDK sends at
most 500 items (and about 4 MiB of JSON) per call, each call one all-or-nothing step, and
all the calls of one bulk call one run: one entry in History, one undo. A member never
splits, and an empty list sends nothing.

```sh
cd python && uv run --extra dev pytest          # against an in-process fake /v1 server
cd typescript && bun test && bunx tsc --noEmit
```

The developer docs' examples run both SDKs against a real API server
(`t test docs-examples`): every round trip shown there is a whole program's.
