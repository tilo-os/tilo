# tilo-sdk (Python SDK)

Drive Tilo workspaces from Python: the same resources, actions, runs, undo
and attribution a person clicking or an agent over MCP gets. Every call is a
route of Tilo's API, `/v1` (`docs/developers/api.md` in the Tilo repository):
JSON over HTTP, the same routes the API server and a Tilo host serve.

The distribution is `tilo-sdk` (`uv add tilo-sdk`); the module it installs is
`tilo`.

```python
import asyncio, pathlib
from tilo import AsyncTilo

async def main():
    async with AsyncTilo() as tilo:                 # key: TILO_API_KEY, API: TILO_API_URL
        applicant = await tilo.workspaces.create("Applicant: Jane Doe")
        ws = tilo.workspace(applicant.id)

        passport = await ws.files.upload("jane-passport.jpg")
        application = await ws.files.upload(pathlib.Path("visa-form.pdf").read_bytes())
        await ws.do("pdf-viewer#import-pdf-content", content=application, name="visa-form.pdf")

        key = await ws.keys.mint(name="jane's agent")   # a key that reaches this one workspace
        print(key.mcp_url, key.token)                   # hand it to the applicant's agent

        async with ws.run("Fill Jane's application"):
            document = await ws.read("pdf-viewer/visa-form.pdf")
            document.form_values["surname"] = "Doe"
            await document.flush()                      # assignments are sent at once; this waits

        async for event in ws.watch("pdf-viewer/visa-form.pdf"):
            if event.kind != "change":
                continue
            print(event.sentence, "by", event.by or event.title)
            if event.title:                             # a change an agent's run made
                break

asyncio.run(main())
```

The sync client has the same surface without `await`: `from tilo import Tilo`.

## How it works

- **One key, one transport.** `AsyncTilo(url=None, key=None)` reads
  `TILO_API_URL` (default `https://api.tilo.sh`) and `TILO_API_KEY`, and sends
  them as `Authorization: Bearer` over httpx. `AsyncTilo.host(origin,
  key_file=…)` instead serves the same `/v1` routes in process, against a Tilo
  host, through the `tilo._native` extension.
- **Account and workspaces.** `GET /v1/workspaces` lists what the key reaches,
  `POST /v1/workspaces` makes one, `DELETE /v1/workspaces/{id}` deletes one
  with everything in it. `GET /v1/account` is the person's plan and what each
  workspace keeps.
- **Actions are invocations.** `await ws.do("notes/Todo#append-paragraph",
  text="Buy milk")` answers the result, or a `Run` if the host has not
  finished: `await run.result()`, `run.output()`, `await run.cancel()`,
  `await run.undo()`.
- **A refusal is an exception.** `Refused(code, message, next_steps)` says
  what the API said and which calls would work; a call that waits for the
  person raises `AskPending(url, run)` and `await ask.wait()` answers once they
  decide; a key the API turns away raises `KeyRevoked`, `KeyExpired` or
  `KeyRefused`.
- **A busy host is waited for.** A call the host refuses because the person
  sends changes faster than it takes them ran nothing, so the client asks
  again after the wait the host says (`retry_after_ms`), with jitter, and with
  the same `Idempotency-Key`, for up to `retry_deadline` seconds
  (`AsyncTilo(retry_deadline=60)`); past it, `RateLimited`, a `Refused`, says
  when to try again.
- **Runs group calls.** `async with ws.run("Fill Jane's application") as run:`
  puts every call inside into one run (`Tilo-Run`, `Tilo-Run-Title`): one undo
  step per app, named in history.
- **Links for people.** A run keeps the link to each record its calls changed:
  `run.links`, and `run.link` when it changed one record. `await ws.link(path)`
  answers the link to anything a path names. It opens the record in its app
  for the workspace's members and carries no secret.
- **Usage.** `await tilo.usage()` reads the person's account with a key
  reaching all their workspaces: per workspace, the VM time it ran and what it
  keeps (`usage.vm_seconds`, `usage.running`).
- **Reads are snapshots, watches are live.** `await ws.read(path)` is a
  snapshot; `async for event in ws.watch(path)` follows changes, runs and
  presence over the workspace's `events` stream, its cursor kept.

## Resources as objects

```python
document = await ws.read("pdf-viewer/visa-form.pdf")
document.page_count                     # a field, from the resource's read
document.form_values["surname"] = "Doe" # a field edit: <path>#field-edit
await document.flush()                  # awaits the edits on their way
await document.read_form_fields()       # an action: <path>#read-form-fields
```

A method is the resource's action, its name in kebab case. An assignment (or
`document.edit("form_values", "surname", value="Doe")`) is the generic
`field-edit` action every resource publishes:
`{"edit": {"path": [{"field": "form_values"}, {"key": "surname"}], "op":
"set", "value": "Doe"}}`. Items are named as steps too:
`await document.edit("blocks", {"item": "c01e"}, "text", value="Buy oat milk")`.

The types of the built-in apps ship with the package: one module per app,
declaring one class per resource kind over these carriers
(`from tilo.types.pdf_viewer import Document`). Each class names the surface
its types were written for, and its calls say so (`Tilo-Surface`): a workspace
whose app publishes another surface refuses them as `SurfaceChanged`, and
`tilo sdk gen` writes them again. `tilo sdk gen --lang python -w <workspace>`
writes a workspace's own apps' types, and `--out` names the directory they go
in. Without generated types everything still works through `ws.resource(path)`
and `ws.do(path, args)`.

A path is written as its segments, and a name holding a `/` keeps its escape:
`ws.read("notes/Q3%2F4%20plan")`.

## End to end

`examples/visa_proof.py` is a visa startup's backend: one workspace per
applicant, a key minted for each applicant's agent, the agent filling a PDF
form and writing a cover letter, and a reviewer reading History, undoing the
agent's run and checking no VM ran. It runs against any API server with a
server key, or against a relay with its app workers on loopback:

```sh
cargo run -p tilo-agent-participant --example api-on-relay -- env VISA_REVOKE_SERVER_KEY=1 \
    uv run --project sdk/python python sdk/python/examples/visa_proof.py
```

## Tests

```sh
uv run --extra dev pytest         # against an in-process fake /v1 server
TILO_TEST_API_URL=http://127.0.0.1:8080 TILO_TEST_KEY=tilo_sk_… uv run pytest tests/test_integration.py
```
