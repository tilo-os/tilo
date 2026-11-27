# tilo-sdk

Drive Tilo workspaces from JavaScript and TypeScript: the same resources,
actions, invocations, undo and attribution a person clicking or an agent gets.
The SDK is a client of Tilo's HTTP API, `/v1` ([the API](../../docs/developers/api.md)),
and nothing else.

```sh
bun add tilo-sdk            # or: npm install tilo-sdk
```

```ts
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();                          // this computer's daemon
const ws = tilo.workspace();                      // the workspace it serves

const note = await run(ws, { path: "notes", action: "new-note" }, { title: "Todo" });
console.log(note.result);                         // what the action produced, once it finished

for await (const event of ws.events()) {          // changes, invocations and who is there
  console.log(event.type, event.data);
  break;
}
```

## The client is the API

`Tilo` and `Workspace` are written from the API's description by `tilo sdk gen`:
one method per operation, its parameters, body and answer typed from the
description's schemas. `tilo.getApi()`, `tilo.listAccountResources(…)` and
`tilo.runAccountAction(…)` are the account's; a workspace's are
`ws.listResources`, `ws.readResource`, `ws.viewResource`, `ws.runAction`,
`ws.getInvocation`, `ws.cancelInvocation`, `ws.undoInvocation`,
`ws.invocationOutput`, `ws.askModel`, `ws.runProgram`, `ws.putContent`,
`ws.getContent` and `ws.events`. The types are built with the package and never
committed (`tools/ci/sdk-gen.sh`).

A request goes one of two ways:

- **This computer's daemon** (default). `new Tilo()` reaches the daemon
  through the `tilo-sdk-native` addon, which answers `/v1` in process as
  whoever connected: the person, or an app process the launcher started. An
  app's `tilo.workspace()` with no argument is the workspace it was started in.
  `bun add tilo-sdk-native` installs the addon.
- **A key, by URL.** `new Tilo({ key })` (or `TILO_API_KEY`) sends the same
  requests to `TILO_API_URL` (`https://api.tilo.sh` unless set) with
  `Authorization: Bearer <key>`. A key's client serves no workspace:
  `tilo.workspace("jane-doe")` names one by its id or its name.

```ts
const backend = new Tilo({ key: process.env.TILO_API_KEY });
const made = await run(backend, { path: "account/workspaces", action: "create" }, { name: "Jane Doe" });
const { workspace } = made.result as { workspace: { id: string } };
const minted = await run(backend, { path: "account/keys", action: "mint" }, { workspace: workspace.id });
console.log(minted.result);                       // { token, mcp_url, … }: hand it to Jane's agent
```

## Actions

`ws.runAction({ path, action }, args)` is one request: `200` when it ran,
`202` with `status: "pending"` when it has not finished or waits for the
person. `run(ws, { path, action }, args)` follows it until it finished
(`getInvocation` with `wait`), and the account's actions by asking again with
the same `Idempotency-Key`. Every mutating request carries one, so a request
asked again while the API is briefly unavailable (`503`) runs once.

`await ws.invocation("Fill Jane's application", async (group) => …)` puts every
request inside into one invocation group (`Tilo-Invocation`,
`Tilo-Invocation-Title`): one undo step per app, named in History.
`group.links` (and `group.link` when one record changed) are the records it
changed; `await group.undo()` takes it back.

A refusal throws `Refused`: `code` is what a program branches on
(`not_permitted`, `key_revoked`, `surface_changed`, …), `error` the API's whole
refusal body, `status` the HTTP status.

## Resources as objects

```ts
const document = await ws.resource("pdf-viewer/visa-form.pdf").load();
document.pageCount;                                  // a field, from the resource
(document.formValues as Record<string, unknown>).surname = "Doe"; // a field edit
await document.flush();                              // awaits the edits on their way
await (document.readFormFields as () => Promise<unknown>)();      // an action
```

A method is the resource's action, its name in camelCase from the app's id
(`readFormFields()` runs `read-form-fields`) and answering what it produced. A
field is either spelling: `formValues` and `form_values` both name the app's
`form_values`. An assignment (or `document.edit(["form_values", "surname"],
"Doe")`) is the generic `field-edit` action every resource publishes.
`rows(ws, { path })` reads every page of a listing; `exported(ws)` reads every
resource of a workspace whole.

The types of the built-in apps ship with the package: one module per app
under `tilo-sdk/types`. `tilo sdk gen --lang ts -w <workspace>` writes a
workspace's own apps' types. A generated type names the app's surface in each
call (`Tilo-Surface`): a workspace whose app publishes another surface
refuses it (`surface_changed`) rather than running it into the wrong shape.

## Files, events and the vault

- **Files.** `await ws.upload(bytes)` puts the bytes at their BLAKE3 address
  (`putContent`, in `Content-Range` pieces when large) and answers the
  `tilo.content:` reference an action takes; `await ws.download(reference)`
  gives the bytes back.
- **Events.** `for await (const event of ws.events({ path }))` follows changes,
  invocations and presence as server-sent events; `ws.invocationOutput({
  invocation })` follows one invocation until it ends. `return()` ends either at
  once.
- **The vault**, on a daemon's connection. `ws.vault.webCall({ url, key })`
  makes a request with a key the vault keeps, by `vault.item:` reference, and
  answers the reply with the key struck out; `ws.vault.ask({ kind, belongsTo })`
  asks the person for a secret and answers the reference it is kept under.
  `release` and `find` reach a vault item only from an app the daemon started,
  and `ws.callLane` carries an outside call into a workspace call for one.

## Tests

```sh
bun test                       # the client against a server each test fakes
bun run typecheck              # types
```

`test/daemon.test.ts` is the end-to-end suite: it runs against a real daemon
and skips unless `TILO_RUNTIME_PROFILE` names one (`tools/ci/sdk-native.sh`
starts it in a profile of its own).

`examples/visa_proof.ts` is the whole story against a hosted server or a local
one: a workspace per applicant, an agent that fills a form and writes a letter,
a reviewer who undoes it, and no VM time for any of it.
