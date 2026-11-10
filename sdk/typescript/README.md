# @tilo/sdk

Drive Tilo workspaces from JavaScript and TypeScript: the same resources,
actions, runs, undo and attribution a person clicking or an agent over MCP
gets. No second API: every call here is the `tools/call` an agent makes,
asking for typed answers.

```ts
import { Tilo } from "@tilo/sdk";

const tilo = new Tilo();                          // key: TILO_API_KEY, door: TILO_DOOR
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

- **One key.** `new Tilo({ key, door })` reads `TILO_API_KEY` and
  `TILO_DOOR` (default `https://mcp.tilo.sh`). Send it as
  `Authorization: Bearer`.
- **Account and workspaces.** `/mcp` is the account: list, create and delete
  workspaces, list and mint keys. `/w/<workspace>/mcp` is one workspace.
- **Actions are invocations.** `await ws.do("notes/Todo#append-paragraph",
  { text: "Buy milk" })` answers the result, or a `Run` if the host has not
  finished: `await run.result()`, `run.output()`, `await run.cancel()`,
  `await run.undo()`.
- **A refusal throws.** `Refused(code, message, nextSteps)` says what the host
  said and which calls would work; a call that waits for the person throws
  `AskPending(url, invocationId)` and `await ask.wait()` answers once they
  decide.
- **A busy host is waited for.** A call the host refuses because the person
  sends changes faster than it takes them ran nothing, so the client asks
  again after the wait the host says (`retry_after_ms`), with jitter, for up
  to `retryDeadlineMs` (`new Tilo({ retryDeadlineMs: 60_000 })`); past it,
  `RateLimited`, a `Refused`, says when to try again.
- **Runs group calls.** `await ws.run("Fill Jane's application", async (run) =>
  …)` puts every call inside into one run: one undo step per app, named in
  history.
- **Links for people.** A run keeps the link to each record its calls changed:
  `run.links`, and `run.link` when it changed one record. `await ws.link(path)`
  answers the link to anything a path names. It opens the record in its app
  for the workspace's members and carries no secret.
- **Reads are snapshots, watches are live.** `await ws.read(path)` is a
  snapshot; `for await (const event of ws.watch(path))` follows changes, runs
  and presence, its cursor kept.

## Resources as objects

```ts
const document = await ws.read("pdf-viewer/visa-form.pdf");
document.pageCount;                                  // a field, from tilo_read
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
Without generated types everything still works through `ws.resource(path)`
and `ws.do(path, args)`.

## Tests

```sh
bun test                       # against an in-process fake door
bunx tsc --noEmit              # types
TILO_TEST_DOOR=http://127.0.0.1:8080 TILO_TEST_KEY=tilo_sk_… bun test test/integration.test.ts
```
