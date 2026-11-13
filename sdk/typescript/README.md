# tilo-sdk

Drive Tilo workspaces from JavaScript and TypeScript: the same resources,
actions, runs, undo and attribution a person clicking or an agent gets. No
second API: every call is one route of Tilo's `/v1` JSON API
([the API](../../docs/developers/api.md)), with your key.

```sh
bun add tilo-sdk            # or: npm install tilo-sdk
```

```ts
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();                          // key: TILO_API_KEY, API: TILO_API_URL
const applicant = await tilo.workspaces.create("Applicant: Jane Doe");
const ws = tilo.workspace(applicant.id);

const passport = await ws.files.upload(await Bun.file("jane-passport.jpg").bytes());
const application = await ws.files.upload(await Bun.file("visa-form.pdf").bytes());
await ws.do("pdf-viewer#import-pdf-content", { content: application, name: "visa-form.pdf" });

const key = await ws.keys.mint("jane's agent");    // a key that reaches this one workspace
console.log(key.mcpUrl, key.token);                // hand it to the applicant's agent

await ws.run("Fill Jane's application", async () => {
  const document = await ws.read("pdf-viewer/visa-form.pdf");
  (document.formValues as Record<string, unknown>).surname = "Doe";
  await document.flush();                          // assignments are sent at once; this waits
});

for await (const event of ws.watch("pdf-viewer/visa-form.pdf")) {
  if (event.kind !== "change") continue;
  console.log(event.sentence, "by", event.by ?? event.run);   // a change, as it lands
  if (event.run !== null) break;                   // one the applicant's agent made
}
```

## How it works

- **One key.** `new Tilo({ url, key })` reads `TILO_API_URL` and
  `TILO_API_KEY` (`https://api.tilo.sh` when neither says). The key goes with
  every call as `Authorization: Bearer`.
- **One API.** Every call is a `/v1` route: `/v1/workspaces`,
  `/v1/workspaces/{workspace}/resources/{path}`, `…/actions/{action}`,
  `…/runs/{run}`, `…/events`. Nothing else is spoken.
- **A host in process.** `await Tilo.host(origin, { keyFile })` (or
  `{ key, cert }`) serves the same routes from a Tilo host directly, through
  the `tilo-sdk-native` addon: `bun add tilo-sdk-native`, or point a client at
  an API server instead.
- **Actions are invocations.** `await ws.do("notes/Todo#append-paragraph",
  { text: "Buy milk" })` answers the result, or a `Run` if the host has not
  finished: `await run.result()`, `run.output()`, `await run.cancel()`,
  `await run.undo()`.
- **A refusal throws.** `Refused(code, message, nextSteps)` says what the host
  said and which calls would work; a call that waits for the person throws
  `AskPending(url, invocationId)` and `await ask.wait()` answers once they
  decide. A key that was revoked or expired throws `KeyRevoked`/`KeyExpired`.
- **Every change is idempotent.** Each POST, PUT or DELETE carries an
  `Idempotency-Key`, and a retry carries the same one: a call the host is busy
  with (`RateLimited`, `retry_after_ms`) or one that never arrived is asked
  again until the client's retry deadline, and the host runs it once.
- **Runs group calls.** `await ws.run("Fill Jane's application", async (run) =>
  …)` puts every call inside into one run: one undo step per app, named in
  history.
- **Links for people.** A run keeps the link to each record its calls changed:
  `run.links`, and `run.link` when it changed one record. `await ws.link(path)`
  answers the link to anything a path names. It opens the record in its app
  for the workspace's members and carries no secret.
- **Reads are snapshots, watches are live.** `await ws.read(path)` is a
  snapshot; `for await (const event of ws.watch(path))` follows changes, runs
  and presence as they happen, its cursor kept (`Last-Event-ID`).

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
"Doe")`) is the generic `field-edit` action every resource publishes:
`{ edit: { path: [{ field: "form_values" }, { key: "surname" }], op: "set",
value: "Doe" } }`. Items are named as steps too:
`document.edit(["blocks", { item: "c01e" }, "text"], "Buy oat milk")`.

The types of the built-in apps ship with the package: one module per app,
declaring one class or interface per resource kind over these carriers
(`pdfViewer.Document`). `tilo sdk gen --lang ts -w <workspace>` writes a
workspace's own apps' types, and `--out` names the directory they go in.
A generated type names the app's surface, and a call through one sends it as
`Tilo-Surface`: a workspace whose app publishes another surface refuses it
(`SurfaceChanged`) rather than running it into the wrong shape, and
`tilo sdk gen` writes the types again. Without generated types everything
still works through `ws.resource(path)` and `ws.do(path, args)`.

## Tests

```sh
bun test                       # against an in-process fake /v1 server
bunx tsc --noEmit              # types
TILO_TEST_API_URL=http://127.0.0.1:8080 TILO_TEST_KEY=tilo_sk_… bun test test/integration.test.ts
```

`examples/visa_proof.ts` is the whole story against a real server: a workspace
per applicant, an agent that fills a form and writes a letter, a reviewer who
undoes it, and no VM time for any of it.
