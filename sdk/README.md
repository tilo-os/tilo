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
as a typed object over a read and `field-edit`, `Refused`, `AskPending` and
`ProtocolError`, `SurfaceChanged` when an app's surface moved past the
generated types, and for a key the API turns away `KeyRevoked`, `KeyExpired`
or `KeyRefused`.

A key is `TILO_API_KEY` and the API server is `TILO_API_URL`
(`https://api.tilo.sh` by default). `tilo sdk gen` writes per-app types into
`python/src/tilo/types/` and `typescript/src/types/`; both ship the proxies
those types stand on.

```sh
cd python && uv run --extra dev pytest          # against an in-process fake /v1 server
cd typescript && bun test && bunx tsc --noEmit
```

Both suites also take a real API server: set `TILO_TEST_API_URL` and
`TILO_TEST_KEY` to run `tests/test_integration.py` or
`test/integration.test.ts` against it.
