# Quickstart

Five minutes: sign in, make a workspace, connect your agent, and make your first call from Python or
TypeScript. Everything below works on the same workspace, from every way in.

## 1. Sign in and make a workspace

Open [tilo.sh](https://tilo.sh) and sign in. In the workspace overview, choose **New workspace** and
name it `First steps`. You are in it now: a desk with its apps, and nothing else of anyone's.

> [!NOTE]
> A workspace can also be made without touching the page: `tilo account workspaces create "First
> steps"`, or `tilo.workspaces.create("First steps")` from either SDK. The page, the command and the
> SDKs make the same workspace.

## 2. Connect your agent

Give your agent the MCP address. In Claude Code, one line:

```sh
claude mcp add --transport http tilo https://mcp.tilo.sh
```

The first time the agent uses it, it registers itself, and your Tilo asks you once: this agent, these
workspaces, until this date. Approve it, and the agent can list your workspaces and work in them.

That address reaches every workspace of yours. To point an agent at one workspace, use its own address
instead: `https://mcp.tilo.sh/w/<workspace>/mcp`.

## 3. Get a key for your code

Your code needs a key, not a browser. Ask for one:

```sh
curl -fsSL https://tilo.sh | sh   # the `tilo` command, into ~/.local/bin
tilo keys create "My backend"
```

It prints a link. Open it, allow the key in your Tilo, and the command prints the key once:

```text
Approved. Your key (shown once):
export TILO_API_KEY=tilo_sk_…
```

Keep it in your secret store. A key that reaches all your workspaces works until you revoke it
(`tilo keys revoke <grant>`), so a backend does not break on a date. You can also make keys, and revoke
them, on [your grants page](https://mcp.tilo.sh/grants).

## 4. Make your first call

```sh
uv add tilo-sdk        # Python:  from tilo import Tilo
bun add tilo-sdk       # TypeScript:  import { Tilo } from "tilo-sdk"
```

The SDKs read `TILO_API_KEY` from the environment, so nothing is hard-coded. This makes a workspace of
its own, lists the apps in it, writes a note and reads it back:

```python example=first-workspace
from tilo import Tilo

tilo = Tilo()                                         # the key in TILO_API_KEY
created = tilo.workspaces.create("First steps")       # or reuse one you have
workspace = tilo.workspace(created.id)

print("workspace:", created.name, created.id)
print("apps:", len(workspace.ls().rows))

workspace.do("notes#new-note", title="Hello")
note = workspace.read("notes/Hello")
print("note:", note.kind, "title:", note.fields["title"])
```

```typescript example=first-workspace
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();                                     // the key in TILO_API_KEY
const created = await tilo.workspaces.create("First steps"); // or reuse one you have
const workspace = tilo.workspace(created.id);

console.log("workspace:", created.name, created.id);
const apps = await workspace.ls();
console.log("apps:", apps.rows.length);

await workspace.do("notes#new-note", { title: "Hello" });
const note = await workspace.read("notes/Hello");
console.log("note:", note.kind, "title:", note.fields.title);
```

Open the workspace at [tilo.sh](https://tilo.sh), and the note is there, with the run that made it in
its history.

## Where next

- [Workspaces](workspaces.md) and [resources and actions](resources-and-actions.md): the two ideas the
  rest builds on.
- [One workspace per customer](../developers/one-workspace-per-customer.md): a workspace and a key per
  customer, for your backend.
- [Identities and keys](identities.md#keys): what a key reaches, and how to narrow it.
