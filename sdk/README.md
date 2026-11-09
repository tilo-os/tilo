# Tilo SDKs

Script a Tilo workspace from your own code. Both packages make the same
`tools/call`s an agent makes, asking for typed answers instead of agent text:
the same resources, actions, permissions, history and undo. There is no
second API. The design is `docs/developers/sdk.md` and the wire is
`docs/developers/sdk-wire.md`.

- `python/`: the `tilo` package (`AsyncTilo` and the sync `Tilo`). uv project,
  Python 3.11 or newer, `httpx` for HTTP.
- `typescript/`: the `@tilo/sdk` package, Promise-only. bun project, ESM,
  `fetch`.

Both answer the same surface, `snake_case` in Python and `camelCase` in
TypeScript: `workspaces.list/create/delete`, `keys.list/create/mint/revoke`,
`ws.ls/read/find/do/program/status/watch`, `ws.files.upload`,
`ws.keys.mint`, `ws.mint_mcp`, `ws.run(title)` for one undo step per app,
`ws.resource(path)` as a typed object over `tilo_read` and `field-edit`,
`Refused`, `AskPending` and `ProtocolError`.

A key is `TILO_API_KEY` and the door is `TILO_DOOR` (default
`https://mcp.tilo.sh`). `tilo sdk gen` writes per-app types into
`python/src/tilo/types/` and `typescript/src/types/`; both ship the proxies
those types stand on.

```sh
cd python && uv run pytest                      # against an in-process fake door
cd typescript && bun test && bunx tsc --noEmit
```

Both suites also take a real door: set `TILO_TEST_DOOR` and `TILO_TEST_KEY`
to run `tests/test_integration.py` or `test/integration.test.ts` against it.
