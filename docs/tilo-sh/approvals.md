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

When an action requires approval, the API answers **pending** rather than an error, with the
invocation it runs under and the `ask` the person answers. `run` follows it until they decide.

Nothing executes while the invocation is pending. Retrying the same request with the same
idempotency key answers the existing pending invocation rather than duplicating it. Once approved,
the invocation completes; if denied, it fails and nothing executes.

## Example: requesting an API key

Requesting a new API key asks the person, and `run` answers it once they approve it on a device of
theirs:

```python example=ask-for-key
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        # Making a key asks the person: the call answers `pending` with the ask until they
        # approve it on a device of theirs, and `run` asks again with the same Idempotency-Key.
        answer = await run(
            tilo,
            {"path": "account/keys", "action": "create"},
            {"name": "Nightly job", "expires_in": 30 * 24 * 60 * 60},
        )
        made = answer.get("result") or {}
        print("a key was made:", bool(made.get("token") or made.get("key")))


asyncio.run(main())
```

```typescript example=ask-for-key
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();

// Making a key asks the person: the request answers `pending` with the ask until they approve
// it on a device of theirs, and `run` asks again with the same Idempotency-Key.
const answer = await run(
  tilo,
  { path: "account/keys", action: "create" },
  { name: "Nightly job", expires_in: 30 * 24 * 60 * 60 },
);
const made = (answer.result ?? {}) as { token?: string; key?: string };
console.log("a key was made:", Boolean(made.token ?? made.key));
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
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo,
            {"path": "account/workspaces", "action": "create"},
            {"name": "Release checks template"},
        )
        workspace = made["result"]["workspace"]["id"]

        # Turning it on asks the person on their approving device, whichever key asks.
        print(
            "now:",
            (
                await run(
                    tilo,
                    {"path": "account/workspaces", "action": "unattended"},
                    {"workspace": workspace, "on": True},
                )
            ).get("result"),
        )

        # Turning it off again asks nobody.
        print(
            "off:",
            (
                await run(
                    tilo,
                    {"path": "account/workspaces", "action": "unattended"},
                    {"workspace": workspace, "on": False},
                )
            ).get("result"),
        )


asyncio.run(main())
```

```typescript example=run-unattended
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Release checks template" },
);
const workspace = (made.result as { workspace: { id: string } }).workspace.id;

// Turning it on asks the person on their approving device, whichever key asks.
const on = await run(
  tilo,
  { path: "account/workspaces", action: "unattended" },
  { workspace, on: true },
);
console.log("now:", on.result);

// Turning it off again asks nobody.
const off = await run(
  tilo,
  { path: "account/workspaces", action: "unattended" },
  { workspace, on: false },
);
console.log("off:", off.result);
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
- [Errors](errors.md): a refusal among the rest, and a pending answer that is not one.
