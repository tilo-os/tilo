# Quickstart

Create a workspace, connect an agent, and invoke your first action from Python or TypeScript.
The web app, CLI, MCP server, and SDKs all operate on the same workspace.

## 1. Sign in and create a workspace

Open [tilo.sh](https://tilo.sh) and sign in. In the workspace overview, choose **New workspace** and
name it `First steps`.

> [!NOTE]
> You can also create a workspace from the CLI (`tilo do account/workspaces#create name="First steps"`) or SDK
> (`run(tilo, {"path": "account/workspaces", "action": "create"}, {"name": "First steps"})`). Once a
> workspace is set up the way you want, branch it
> instead of starting over: `tilo do account/workspaces#branch workspace="First steps" name="Second try"`
> ([branch a workspace](workspaces.md#branch-a-workspace)).

## 2. Connect your agent

Add the Tilo MCP server to your agent, as a streamable HTTP server named `tilo` at
`https://mcp.tilo.sh`. In an agent that reads a JSON MCP configuration:

```json
{"mcpServers": {"tilo": {"type": "http", "url": "https://mcp.tilo.sh"}}}
```

On the first call, the agent registers itself and Tilo prompts you once to approve the agent, its
workspace scope, and its expiration date.

`https://mcp.tilo.sh` grants access to all of your workspaces. To scope an agent to a single
workspace, use `https://mcp.tilo.sh/w/<workspace>/mcp` instead.

## 3. Create an API key for your code

Scripts and backends authenticate with an API key instead of a browser session:

```sh
curl -fsSL https://tilo.sh | sh   # installs the `tilo` CLI into ~/.local/bin
tilo login                        # this computer as one of your devices, approved in Tilo
tilo do account/keys#create name="My script" --out my-script.key
```

The command prints an approval link. Open it and approve the key in Tilo; the key is written to
`my-script.key`, which only you can read, and never printed:

```sh
export TILO_API_KEY=$(cat my-script.key)   # what the SDKs read
```

Store it in your secret manager. Account-wide keys remain valid until revoked
(`tilo do "account/keys/<grant>#revoke"`). You can also create and revoke keys on
[your grants page](https://mcp.tilo.sh/grants).

## 4. Make your first call

```sh
uv add tilo-sdk        # Python:  from tilo import Tilo
bun add tilo-sdk       # TypeScript:  import { Tilo } from "tilo-sdk"
```

The SDKs read `TILO_API_KEY` from the environment, so nothing is hard-coded. This makes a workspace of
its own, lists the apps in it, writes a snippet and reads it back:

```python example=first-workspace
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:               # the key in TILO_API_KEY
        made = await run(                  # or reuse one you have
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "First steps"}
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])

        print("workspace:", made["result"]["workspace"]["name"], await workspace.id())
        print("apps:", len((await workspace.list_resources())["rows"]))

        await run(workspace, {"path": "snippets", "action": "new-snippet"}, {"title": "Hello"})
        snippet = await workspace.resource("snippets/Hello").load()
        print("snippet:", snippet.kind, "title:", snippet.fields["title"])


asyncio.run(main())
```

```typescript example=first-workspace
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();                                     // the key in TILO_API_KEY
const made = await run(                                     // or reuse one you have
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "First steps" },
);
const created = (made.result as { workspace: { id: string; name: string } }).workspace;
const workspace = tilo.workspace(created.id);

console.log("workspace:", created.name, await workspace.id());
const apps = await workspace.listResources();
console.log("apps:", apps.rows.length);

await run(workspace, { path: "snippets", action: "new-snippet" }, { title: "Hello" });
const snippet = await workspace.resource("snippets/Hello").load();
console.log("snippet:", snippet.kind, "title:", snippet.fields.title);
```

```curl example=first-workspace
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"   # the key in TILO_API_KEY

# The same Idempotency-Key again answers this workspace rather than making another.
created=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Idempotency-Key: first-steps" \
  -H "Content-Type: application/json" -d '{"name": "First steps"}' | jq -r .result.workspace)
workspace=$(jq -r .id <<<"$created")
echo "workspace: $(jq -r .name <<<"$created") $workspace"

apps=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources" -H "$auth")
echo "apps: $(jq '.rows | length' <<<"$apps")"

curl -sS --fail-with-body "$api/workspaces/$workspace/resources/snippets/actions/new-snippet" -H "$auth" \
  -H "Content-Type: application/json" -d '{"title": "Hello"}' > /dev/null
snippet=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources/snippets/Hello" -H "$auth")
echo "snippet: $(jq -r .kind <<<"$snippet") title: $(jq -r .fields.title <<<"$snippet")"
```

Open the workspace at [tilo.sh](https://tilo.sh), and the snippet is there, with the run that made it in
its history.

## Where next

- [Workspaces](workspaces.md) and [resources and actions](resources-and-actions.md): the two ideas the
  rest builds on.
- [Approvals](approvals.md) and [the hosted Browser](hosted-browser.md): human-in-the-loop actions.
- [Identities and keys](identities.md#keys): what a key reaches, and how to narrow it.
