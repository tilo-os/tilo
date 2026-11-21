# Quickstart

Create a workspace, connect an agent, and invoke your first action from Python or TypeScript.
The web app, CLI, MCP server, and SDKs all operate on the same workspace.

## 1. Sign in and create a workspace

Open [tilo.sh](https://tilo.sh) and sign in. In the workspace overview, choose **New workspace** and
name it `First steps`.

> [!NOTE]
> You can also create a workspace from the CLI (`tilo workspace create "First steps"`) or SDK
> (`tilo.workspaces.create("First steps")`). Once a workspace is set up the way you want, branch it
> instead of starting over: `tilo workspace branch "First steps" "Second try"`
> ([branch a workspace](workspaces.md#branch-a-workspace)).

## 2. Connect your agent

Add the Tilo MCP server to your agent. In Claude Code:

```sh
claude mcp add --transport http tilo https://mcp.tilo.sh
```

On the first call, the agent registers itself and Tilo prompts you once to approve the agent, its
workspace scope, and its expiration date.

`https://mcp.tilo.sh` grants access to all of your workspaces. To scope an agent to a single
workspace, use `https://mcp.tilo.sh/w/<workspace>/mcp` instead.

## 3. Create an API key for your code

Scripts and backends authenticate with an API key instead of a browser session:

```sh
curl -fsSL https://tilo.sh | sh   # installs the `tilo` CLI into ~/.local/bin
tilo keys create "My script"
```

The command prints an approval link. Open it, approve the key in Tilo, and the CLI prints the key
once:

```text
Approved. Your key (shown once):
export TILO_API_KEY=tilo_sk_…
```

Store it in your secret manager. Account-wide keys remain valid until revoked
(`tilo keys revoke <grant>`). You can also create and revoke keys on
[your grants page](https://mcp.tilo.sh/grants).

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

```curl example=first-workspace
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"   # the key in TILO_API_KEY

# The same Idempotency-Key again answers this workspace rather than making another.
created=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Idempotency-Key: first-steps" \
  -H "Content-Type: application/json" -d '{"name": "First steps"}')
workspace=$(jq -r .id <<<"$created")
echo "workspace: $(jq -r .name <<<"$created") $workspace"

apps=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources" -H "$auth")
echo "apps: $(jq '.rows | length' <<<"$apps")"

curl -sS --fail-with-body "$api/workspaces/$workspace/resources/notes/actions/new-note" -H "$auth" \
  -H "Content-Type: application/json" -d '{"title": "Hello"}' > /dev/null
note=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources/notes/Hello" -H "$auth")
echo "note: $(jq -r .kind <<<"$note") title: $(jq -r .fields.title <<<"$note")"
```

Open the workspace at [tilo.sh](https://tilo.sh), and the note is there, with the run that made it in
its history.

## Where next

- [Workspaces](workspaces.md) and [resources and actions](resources-and-actions.md): the two ideas the
  rest builds on.
- [Approvals](approvals.md) and [the hosted Browser](hosted-browser.md): human-in-the-loop actions.
- [Identities and keys](identities.md#keys): what a key reaches, and how to narrow it.
