# tilo-sdk

Drive Tilo workspaces from JavaScript and TypeScript: the same resources,
actions, invocations, undo and attribution a person clicking or an agent gets.
Every call is one tool of Tilo's agent surface (`tilo_ls`, `tilo_read`,
`tilo_do`, …), carried by one of two transports.

```sh
bun add tilo-sdk            # or: npm install tilo-sdk
```

```ts
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();                          // this computer's daemon
const ws = tilo.workspace();                      // the workspace it serves

const note = await ws.do("notes#new-note", { title: "Todo" });
console.log(note);                                // its result, or an Invocation to follow

for await (const event of ws.watch()) {           // changes, invocations and who is there
  console.log(event.kind, event.sentence ?? event.phase);
  break;
}
```

## The two transports

- **This computer's daemon** (default). `new Tilo()` reaches the daemon over
  its native endpoint through the `tilo-sdk-native` addon. The daemon admits
  the connection as whoever connects: the person, or an app process the
  launcher started (it hands that process a connected descriptor, which the
  addon reads itself). An app's `tilo.workspace()` with no argument is the one
  workspace it was started in. `bun add tilo-sdk-native` installs the addon.
- **A key, for a caller with no daemon.** `new Tilo({ key })` (or
  `TILO_API_KEY`) sends the same tool calls to the hosted server
  (`TILO_API_URL` or `https://api.tilo.sh`) as MCP `tools/call` over HTTPS.
  That is the only HTTP the SDK speaks, and it is not `/v1`. A key's client
  serves no workspace: `tilo.workspace("name")` names one.

```ts
const backend = new Tilo({ key: process.env.TILO_API_KEY });
const ws = backend.workspace("jane-doe");
const key = await ws.keys.mint("jane's agent");
console.log(key.mcpUrl, key.token);               // hand it to the applicant's agent
```

`new Tilo({ transport })` takes your own `Transport`: anything with
`workspace(): Promise<string | null>` and
`call(workspace, tool, args, group): Promise<object>`.

## Resources as objects

```ts
const document = await ws.read("pdf-viewer/visa-form.pdf");
document.pageCount;                                  // a field, from the resource
(document.formValues as Record<string, unknown>).surname = "Doe"; // a field edit
await document.flush();                              // awaits the edits on their way
await (document.readFormFields as () => Promise<unknown>)();      // an action
```

A method is the resource's action, its name in camelCase from the app's
kebab-case id (`readFormFields()` runs `read-form-fields`). A field is
either spelling: `formValues` and `form_values` both name the app's
`form_values`. An assignment (or `document.edit(["form_values", "surname"],
"Doe")`) is the generic `field-edit` action every resource publishes.

The types of the built-in apps ship with the package: one module per app
under `tilo-sdk/types`. `tilo sdk gen --lang ts -w <workspace>` writes a
workspace's own apps' types. A generated type names the app's surface, and a
call through one checks it: a workspace whose app publishes another surface
refuses it (`SurfaceChanged`) rather than running it into the wrong shape.

## What else the client does

- **Actions are invocations.** `await ws.do("notes/Todo#append-paragraph",
  { text: "Buy milk" })` answers the result, or an `Invocation` if the host
  has not finished: `await invocation.result()`, `invocation.output()`,
  `await invocation.cancel()`, `await invocation.undo()`.
- **Invocations group calls.** `await ws.invocation("Fill Jane's application",
  async (invocation) => …)` puts every call inside into one group: one undo
  step per app, named in history. `invocation.links` (and `link` when one
  record changed) are the links to what it changed.
- **A refusal throws.** `Refused(code, message, nextSteps)` says what the host
  said and which calls would work. A call that waits for the person throws
  `AskPending`; `await ask.wait()` answers once they decide. A key the host
  turns away throws `KeyRevoked`, `KeyExpired` or `KeyRefused`; an unreachable
  host throws `Unreachable`, and any other wire failure `ProtocolError`.
- **Files.** `await ws.files.upload(bytes)` answers a `tilo.content:<hash>`
  reference an action takes; `await ws.files.download(reference)` gives the
  bytes back.
- **Watches are live.** `for await (const event of ws.watch(path))` follows
  changes, invocations and presence, its cursor kept across reconnects.
- **The vault.** `ws.vault.webCall({ url, key })` makes a request with a key
  the vault keeps, by `vault.item:` reference, and answers the reply with the
  key struck out. Its body is `body` text, `bodyParts` (inline text or JSON,
  and `content` parts naming bytes the workspace keeps by their BLAKE3 hex
  address, with a filename and content type), or `bodyContent`, one content
  reference sent as the whole body; the vault reads those bytes from the
  workspace's store, so they never pass through the program. `ws.vault.ask({
  kind, belongsTo })` asks the person for a secret and answers the reference it
  is kept under. No secret comes back to the program.

## Tests

```sh
bun test                       # the SDK's logic, without a daemon
bun run typecheck              # types
```

`test/daemon.test.ts` is the end-to-end suite: it runs against a real daemon
and skips unless `TILO_RUNTIME_PROFILE` names one (`tools/ci/sdk-native.sh`
starts it in a profile of its own).

`examples/visa_proof.ts` is the whole story against a hosted server or a local
one: a workspace per applicant, an agent that fills a form and writes a letter,
a reviewer who undoes it, and no VM time for any of it.
