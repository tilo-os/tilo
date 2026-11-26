# Tilo SDKs

Script a Tilo workspace from your own code: the same resources, actions,
permissions, history and undo a person clicking or an agent over MCP gets.
There is no second API. The design is `docs/developers/sdk.md`.

- `python/`: the `tilo-sdk` package (`AsyncTilo` and the sync `Tilo`), imported as `tilo`.
  uv project, Python 3.11 or newer.
- `typescript/`: the `tilo-sdk` package, Promise-only. bun project, ESM.

```sh
uv add tilo-sdk          # Python:        from tilo import Tilo
bun add tilo-sdk         # TypeScript:    import { Tilo } from "tilo-sdk"
```

Both call the tools an agent calls (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_watch`, ...),
answered typed. On a computer that runs Tilo they reach its daemon through their native part
(`python/native`, `typescript/native`): the line wire's Rust client, the one `tilo` uses, carrying
action frames on the daemon's native endpoint, admitted as the person, or as the app the daemon
started the program as. Without a daemon, a key (`TILO_API_KEY`) sends the same calls to the API
server's MCP endpoint (`TILO_API_URL`, `https://api.tilo.sh` by default).

Both answer the same surface, `snake_case` in Python and `camelCase` in
TypeScript: `workspaces.list/create/delete`, `keys.list/create/mint/revoke`,
`ws.ls/read/find/do/link/program/status/watch`, `ws.files.upload`,
`ws.keys.mint`, `ws.vault.web_call/ask`, `ws.run(title)` for one undo step per app, `ws.resource(path)`
as a typed object over a read and `field-edit`, `res.many(action, items)` for several
actions as one step per call, `Refused`, `AskPending` and
`ProtocolError`, `SurfaceChanged` when an app's surface moved past the
generated types, `Unreachable` when Tilo cannot be reached, and for a key
that is turned away `KeyRevoked`, `KeyExpired` or `KeyRefused`.

`tilo sdk gen` writes per-app types into
`python/src/tilo/types/` and `typescript/src/types/` when a package is built or
tested; they are not committed. `bun run types` in `typescript/` writes both
from this checkout (`tools/ci/sdk-gen.sh`). Both packages ship the types and
the proxies they stand on.

A record action that changes it and answers nothing also has a bulk form. Generated types
expose it as `create_many` (Python) and `createMany` (TypeScript), over a typed `CreateArgs`
list; `res.many("create", items)` is the same without generated types. The SDK sends at
most 500 items (and about 4 MiB of JSON) per call, each call one all-or-nothing step, and
all the calls of one bulk call one run: one entry in History, one undo. A member never
splits, and an empty list sends nothing.

```sh
cd python && uv run --extra dev pytest          # what needs no daemon
cd typescript && bun test && bunx tsc --noEmit
t test sdk-native                               # both, end to end, against a daemon of their own
```
