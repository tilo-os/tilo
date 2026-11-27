# tilo-sdk (Python SDK)

Drive Tilo workspaces from Python: the same resources, actions, invocations,
undo and attribution a person clicking or an agent gets. The SDK is a client of
Tilo's HTTP API, `/v1` ([the API](../../docs/developers/api.md)), and nothing
else.

```sh
uv add tilo-sdk
```

```python
import asyncio
from tilo import Tilo, run

async def main():
    async with Tilo() as tilo:                     # this computer's daemon
        ws = tilo.workspace()                       # the workspace it serves

        note = await run(ws, {"path": "notes", "action": "new-note"}, {"title": "Todo"})
        print(note["result"])                        # what the action produced

        async for event in ws.events():              # changes, invocations and who is there
            print(event["type"], event["data"])
            break

asyncio.run(main())
```

## The client is the API

`Tilo` and `Workspace` are written from the API's description by `tilo sdk
gen`: one method per operation, its parameters, body and answer typed from the
description's schemas. `tilo.get_api()`, `tilo.list_account_resources(…)` and
`tilo.run_account_action(…)` are the account's; a workspace's are
`ws.list_resources`, `ws.read_resource`, `ws.view_resource`, `ws.run_action`,
`ws.get_invocation`, `ws.cancel_invocation`, `ws.undo_invocation`,
`ws.invocation_output`, `ws.ask_model`, `ws.run_program`, `ws.put_content`,
`ws.get_content` and `ws.events`. The types are built with the package and never
committed (`tools/ci/sdk-gen.sh`).

A request goes one of two ways:

- **This computer's daemon** (default). `Tilo()` reaches the daemon through the
  `tilo._native` extension, which answers `/v1` in process as whoever
  connected: the person, or an app process the launcher started. An app's
  `tilo.workspace()` with no argument is the workspace it was started in.
- **A key, by URL.** `Tilo(key=...)` (or `TILO_API_KEY`) sends the same requests
  to `TILO_API_URL` (`https://api.tilo.sh` unless set) with
  `Authorization: Bearer <key>`. A key's client serves no workspace:
  `tilo.workspace("jane-doe")` names one by its id or its name.

```python
backend = Tilo()
made = await run(backend, {"path": "account/workspaces", "action": "create"}, {"name": "Jane Doe"})
workspace = made["result"]["workspace"]
minted = await run(backend, {"path": "account/keys", "action": "mint"}, {"workspace": workspace["id"]})
print(minted["result"])                             # {token, …}: hand it to Jane's agent
```

## Actions

`ws.run_action({"path", "action"}, args)` is one request: `200` when it ran,
`202` with `status: "pending"` when it has not finished or waits for the
person. `run(ws, {"path", "action"}, args)` follows it until it finished
(`get_invocation` with `wait`), and the account's actions by asking again with
the same `Idempotency-Key`. Every mutating request carries one, so a request
asked again while the API is briefly unavailable (`503`) runs once.

`async with ws.invocation("Fill Jane's application") as group:` puts every
request inside into one invocation group (`Tilo-Invocation`,
`Tilo-Invocation-Title`): one undo step per app, named in History.
`group.links` (and `group.link` when one record changed) are the records it
changed; `await group.undo()` takes it back.

A refusal raises `Refused`: `code` is what a program branches on
(`not_permitted`, `key_revoked`, `surface_changed`, …), `error` the API's whole
refusal body, `status` the HTTP status.

## Resources as objects

```python
snippet = await ws.resource("snippets/Hello").load()
snippet.fields["title"]                           # a field, from the resource
snippet.title = "Hello there"                     # a field edit
await snippet.flush()                             # awaits the edits on their way
await snippet.action("star")                      # an action
```

A method is the resource's action, its name in snake case from the app's id
(`read_form_fields()` runs `read-form-fields`) and answering what it produced;
`action("…")` runs one by its id where the types do not name it. An assignment
(or `snippet.edit(["title"], value="Hi")`) is the generic `field-edit` action
every resource publishes. `rows(ws, path=…)` reads every page of a listing;
`exported(ws)` reads every resource of a workspace whole.

The types of the built-in apps ship with the package: one module per app under
`tilo.types`. `tilo sdk gen --lang python -w <workspace>` writes a workspace's
own apps' types. A generated type names the app's surface in each call
(`Tilo-Surface`): a workspace whose app publishes another surface refuses it
(`surface_changed`) rather than running it into the wrong shape.

## Files, events and the vault

- **Files.** `await ws.upload(bytes)` puts the bytes at their BLAKE3 address
  (`putContent`, in `Content-Range` pieces when large) and answers the
  `tilo.content:` reference an action takes; `await ws.download(reference)`
  gives the bytes back.
- **Events.** `async for event in ws.events(path=…)` follows changes,
  invocations and presence as server-sent events;
  `ws.invocation_output(invocation)` follows one invocation until it ends.
  `await events.aclose()` ends either at once.
- **The vault**, on a daemon's connection. `ws.vault.web_call(url, key)`
  makes a request with a key the vault keeps, by `vault.item:` reference, and
  answers the reply with the key struck out; `ws.vault.ask(kind, belongs_to)`
  asks the person for a secret and answers the reference it is kept under.
  `release` and `find` reach a vault item only from an app the daemon started,
  and `ws.call_lane` carries an outside call into a workspace call for one.

## Tests

```sh
uv run --extra dev pytest          # the transport-free tests
```

`tests/test_daemon.py` drives a real daemon and skips unless
`TILO_RUNTIME_PROFILE` names one: `tools/ci/sdk-native.sh test <dir>` starts a
daemon in its own profile and runs it.
