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

The TypeScript SDK is a client of Tilo's HTTP API, `/v1` (`docs/developers/api.md`): `tilo sdk
gen` writes its `Tilo` and `Workspace`, one method per operation, from the API's description. The
Python SDK still calls the tools an agent calls (`tilo_ls`, `tilo_read`, `tilo_do`, ...). On a
computer that runs Tilo both reach its daemon through their native part (`python/native`,
`typescript/native`), admitted as the person, or as the app the daemon started the program as.
Without a daemon, a key (`TILO_API_KEY`) reaches the API server (`TILO_API_URL`,
`https://api.tilo.sh` by default): `/v1` for TypeScript, its MCP endpoint for Python.

`tilo sdk gen` writes per-app types into
`python/src/tilo/types/` and `typescript/src/types/` (with the TypeScript client) when a package
is built or tested; they are not committed. `bun run types` in `typescript/` writes both
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
