# tilo-sdk (Python SDK)

Drive Tilo workspaces from Python: the same resources, actions, invocations,
undo and attribution a person clicking or an agent gets. Every call is one
tool call of Tilo's agent surface (`tilo_ls`, `tilo_read`, `tilo_do`, …).

The distribution is `tilo-sdk` (`uv add tilo-sdk`); the module it installs is
`tilo`.

```python
import asyncio
from tilo import AsyncTilo

async def main():
    async with AsyncTilo() as tilo:                 # this computer's daemon
        ws = tilo.workspace()                       # the workspace it serves
        for row in (await ws.ls()).rows:
            print(row.path, row.label)

        await ws.do("notes/notebook#new-note", title="Buy milk")

        page = await ws.read("notes/notebook")
        print(sorted(page.fields))

        async for event in ws.watch():
            print(event.kind, event.sentence)
            break

asyncio.run(main())
```

The sync client has the same surface without `await`: `from tilo import Tilo`.

## Where the calls go

- **This computer's daemon** (default): `AsyncTilo()` connects through the
  `tilo._native` extension, which speaks the daemon's action frames. It is
  admitted as whoever runs the program: the person, or an app the launcher
  started, in the one workspace that app runs in — which is the workspace
  `tilo.workspace()` means when it is named none.
- **A key, over HTTPS**: `AsyncTilo(key=..., url=...)` reads `TILO_API_KEY` and
  `TILO_API_URL` (`https://api.tilo.sh`), and sends each call as MCP
  `tools/call` to `${url}/mcp`. A key names a workspace in every call, so give
  one: `tilo.workspace("jane-doe")` or `tilo.workspace(id)`.
- **Your own transport**: pass `transport=` anything with `workspace` and
  `call(workspace, tool, arguments, group)`. `Transport` is the protocol it
  follows.

## The surface

- **Reads are snapshots, watches are live.** `await ws.ls(path)` lists the
  apps, an app's resources, or one resource's children and actions;
  `await ws.read(path)` is a snapshot; `async for event in ws.watch(path)`
  follows changes, invocations and presence.
- **Actions are invocations.** `await ws.do("notes/notebook#new-note",
  title="Buy milk")` answers the result, or an `Invocation` if the workspace
  has not finished: `await invocation.result()`, `invocation.output()`,
  `await invocation.cancel()`, `await invocation.undo()`.
- **A refusal is an exception.** `Refused(code, message, next_steps)` says what
  the workspace said and which calls would work; a call that waits for the
  person raises `AskPending(invocation)`, and `await ask.wait()` answers once
  they decide; a key the API turns away raises `KeyRevoked`, `KeyExpired` or
  `KeyRefused`; an unreachable server raises `Unreachable`, and any other wire
  failure raises `ProtocolError`.
- **Invocations group calls.** `async with ws.invocation("Fill the form") as
  group:` puts every call inside into one undo step per app, named in History.
- **Files** go in and out by a `tilo.content:` reference:
  `await ws.files.upload(bytes)` and `await ws.files.download(content)`.
- **The vault** keeps secrets the workspace holds and uses them without
  showing them: `await ws.vault.ask("key", ["https://api.example.com"],
  sent_in="header:X-Key")` answers a `vault.item:…` reference, and
  `await ws.vault.web_call(url, reference)` calls the service with it, the key
  struck out of the answer.
- **The account** is `tilo.workspaces` (list, create, branch, delete, find),
  `tilo.keys` (list, create, mint, revoke) and `tilo.usage()`.

## Resources as objects

```python
document = await ws.read("pdf-viewer/visa-form.pdf")
document.page_count                      # a field, from the resource's read
document.form_values["surname"] = "Doe"  # a field edit: <path>#field-edit
await document.flush()                   # awaits the edits on their way
await document.read_form_fields()        # an action: <path>#read-form-fields
```

A method is the resource's action, its name in kebab case. An assignment is
the generic `field-edit` action every resource publishes. The types of the
built-in apps ship with the package (`from tilo.types.pdf_viewer import
Document`); each class names the surface its types were written for, and a
workspace whose app publishes another one refuses the call as
`SurfaceChanged`. Write them again with `tilo sdk gen --lang python -w
<workspace>`; without generated types everything works through
`ws.resource(path)` and `ws.do(path, args)`.

## Tests

```sh
uv run --extra dev pytest          # the transport-free tests
```

`tests/test_daemon.py` drives a real daemon and skips unless
`TILO_RUNTIME_PROFILE` names one: `tools/ci/sdk-native.sh test <dir>` starts a
daemon in its own profile and runs it.
