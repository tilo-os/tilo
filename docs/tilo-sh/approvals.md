# Approvals

Tilo distinguishes actions that stay inside a workspace from actions with external side effects.
Actions that leave the workspace — such as submitting a web form, sending an email, or creating a
new API key — pause until a person approves them on their own device.

## Which actions require approval

| Action | Requires approval? |
| :--- | :--- |
| Read operations (listing, reading resources, exporting files) | No |
| In-workspace mutations (editing a note, updating a task) | No |
| Creating a workspace or minting a narrower key | No (covered by the parent key's scope) |
| Creating a new top-level API key | Yes |
| Browser page actions (navigate, fill fields, select, click, read) | No, if the key includes browser page scope |
| Shell commands in the workspace VM (`terminal#run`) | Yes, unless unattended commands are enabled on that workspace and the key is scoped to it |
| Enabling unattended commands on a workspace | Yes |
| External side effects (form submissions, uploads, passwords, sending mail) | Yes, always |

## Pending approval lifecycle

When an action requires approval, the API returns a **pending** status rather than an error, along
with an invocation ID and approval URL. Both SDKs raise `AskPending`.

Nothing executes while the invocation is pending. Retrying the same request with the same
idempotency key returns the existing pending invocation rather than duplicating it. Once approved,
the invocation completes with `ok`; if denied, it completes with `failed` and nothing executes.

## Example: requesting an API key

Requesting a new API key raises `AskPending` with an approval link. Calling `wait()` blocks until
the person approves or denies it:

```python example=ask-for-key
from tilo import AskPending, Tilo

tilo = Tilo()

try:
    tilo.keys.create("Nightly job", expires_in=30 * 24 * 60 * 60)
    print("a key was made without asking anyone")
except AskPending as ask:
    print("waiting for the person at", ask.url)
    print("ask:", ask.invocation_id)
    # ask.wait() follows the approval until they decide, and answers the key.
```

```typescript example=ask-for-key
import { AskPending, Tilo } from "tilo-sdk";

const tilo = new Tilo();

try {
  await tilo.keys.create("Nightly job", null, 30 * 24 * 60 * 60);
  console.log("a key was made without asking anyone");
} catch (error) {
  if (!(error instanceof AskPending)) {
    throw error;
  }
  console.log("waiting for the person at", error.url);
  console.log("invocation:", error.invocationId);
  // await error.wait() follows it until they decide, and answers the key.
}
```

## Commands in a workspace's machine

Each command a key runs in a workspace's Linux machine asks the person first, every time. That keeps an
agent from doing anything there alone, which is also what stops one from working on its own in a workspace
made for it.

So a person may let one workspace of their own run commands **unattended**: the keys made for that
workspace alone then run commands in its machine without asking. Turning it on is itself a question: any
key of theirs that reaches the workspace may ask, the call answers pending, and only the person's answer
on their own device turns it on. A key never turns it on for itself, and a refusal leaves it off. Turning it
off asks nobody and holds from the next command.

It holds for that workspace and the branches made from it, which start with it on: allow it once on a
template, branch the template per task, and mint each branch a key that works there alone. Each branch's
setting is its own from then on.

What stays as it was:

- A key reaching all of the person's workspaces, their own backend's included, is asked before each
  command, everywhere.
- Every other action that asks still asks: running a recorded command again, Browser submissions, uploads
  and passwords, mail, opening a Linux app, asking for a key.
- A command that runs unasked is in [History](invocations-and-history.md) as the key's invocation, as an
  asked one is,
  and undone the same way. A workspace whose plan does not cover its machine still refuses the command.

A workspace's listing says `unattended` while it is on.

```python example=run-unattended
from tilo import AskPending, Tilo

tilo = Tilo()
template = tilo.workspaces.create("Release checks template")

try:
    template_ws = tilo.workspace(template.id)
    print("now:", template_ws.unattended())
except AskPending as ask:
    print("waiting for the person to let it run commands unattended")
    print("ask:", ask.invocation_id)
    # ask.wait() follows it until they decide: {"workspace": ..., "unattended": True}.

# Turning it off again asks nobody.
print("off:", tilo.workspace(template.id).unattended(on=False))
```

```typescript example=run-unattended
import { AskPending, Tilo } from "tilo-sdk";

const tilo = new Tilo();
const template = await tilo.workspaces.create("Release checks template");

try {
  console.log("now:", await tilo.workspace(template.id).unattended());
} catch (error) {
  if (!(error instanceof AskPending)) {
    throw error;
  }
  console.log("waiting for the person to let it run commands unattended");
  console.log("ask:", error.invocationId);
  // await error.wait() follows it until they decide: { workspace, unattended: true }.
}

// Turning it off again asks nobody.
console.log("off:", await tilo.workspace(template.id).unattended(false));
```

From the command line, `tilo do account/workspaces#unattended workspace=<workspace> on=true` asks and
waits for the answer; `on=false` turns it off.

## The hosted Browser

The case the design is for: an agent fills a web form in a workspace's hosted Browser, and a person
approves the submission.

Filling asks nobody, when the key carries the page scope: navigate, fill in, select, click, read back,
capture a picture. Then the page is about to send something — a submit button, Enter, the page's own
`submit()`, a `fetch` with a method other than `GET`. The Browser holds the request where it is, and the
call answers that it needs the person, naming what would leave: the page it leaves from, the method, where
it goes, and a picture of the filled page. Their allow releases exactly that request, as filled; a refusal
drops it and the page stays as it was.

The whole flow, with the person answering, is in [the hosted Browser](hosted-browser.md).

> [!NOTE]
> A key that may not operate web pages is asked about every page action, not only the submissions. That is
> the page scope on the key ([keys](identities.md#the-page-scope)), off.

## Where next

- [Invocations and history](invocations-and-history.md): what the approved action recorded.
- [Errors](errors.md): `AskPending` among the rest.
