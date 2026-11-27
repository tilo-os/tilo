# Identities and keys

Every change in a workspace is attributed to an author: a person, one of their devices, or an agent
acting on their behalf. An **API key** lets a script, backend, or agent authenticate with a scoped
grant.

## People and devices

Each person has one root identity and a certified device key per device (laptop, phone, browser).
Adding a new device requires approval from an existing device. API keys work the same way: your
device signs the grant once, so scripts and agents do not need a browser in the loop later.

## Agents

An agent acts on behalf of a person rather than as a separate account. It is capped at the editor
role (it cannot administer the workspace or invite members), and its actions appear under its own
name and color in history and live presence.

Agents authenticate in one of two ways:

- **MCP:** Register at `https://mcp.tilo.sh` (all workspaces) or
  `https://mcp.tilo.sh/w/<workspace>/mcp` (single workspace). Approve once in Tilo; MCP grants
  expire within 90 days.
- **API key:** Use a static `tilo_sk_` token for headless scripts, CI, or background agents.

## Keys

An API key starts with `tilo_sk_`. It is displayed once on creation, and the server stores only its
hash. The prefix allows secret scanners to detect accidental commits.

Choose a scope when creating a key:

| Scope | Permissions | Default lifetime |
| :--- | :--- | :--- |
| **All workspaces** | List and create workspaces, mint scoped keys, and invoke actions in any workspace | Valid until revoked (optional expiry) |
| **Single workspace** | Actions in that workspace only; no account-level calls | Up to 90 days |

An account-wide key is intended for long-lived scripts or services and stays valid until you revoke
it. A single-workspace key isolates an agent or integration to one workspace — it cannot list your
other workspaces, create new ones, or mint additional keys.

### Minting, expiration, and revocation

An account-wide key can **mint** narrower single-workspace keys without prompting for interactive
approval, since the parent key already has access to that workspace. A minted key cannot outlive
its parent, and revoking the parent revokes every child key minted from it.

```sh
tilo do account/keys#mint workspace=project-alpha name="Alpha agent"   # prints MCP URL and token
```

Revoke any key via the CLI or on [your grants page](https://mcp.tilo.sh/grants). Revocation
terminates active sessions immediately; subsequent requests are refused with `key_revoked`
([errors](errors.md)).

### The page scope

By default, a key can drive pages inside a workspace's Browser (navigate, fill fields, select,
click, read back), and keys minted from it inherit that setting. Actions that leave the page —
submitting a form, uploading a file, or entering a password — always require interactive approval
regardless of scope ([approvals](approvals.md#the-hosted-browser)). To require approval for every
browser page action, create it with `browser_pages=false`:

```sh
tilo do account/keys#create name="Read-only agent" workspace=project-alpha browser_pages=false
```

## A key in practice

This mints a key for one workspace, uses it as the agent would, and revokes it:

```python example=key-scopes
import asyncio

from tilo import Refused, Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "One customer"}
        )
        workspace = made["result"]["workspace"]["id"]

        # A key that reaches this workspace and nothing else.
        minted = (
            await run(
                tilo,
                {"path": "account/keys", "action": "mint"},
                {"workspace": workspace, "name": "Their agent"},
            )
        )["result"]
        print("agent's MCP URL:", minted.get("mcp_url"))
        print("it reaches:", minted.get("workspace"))

        # The agent's own client: with that key it sees this workspace's apps.
        agent = Tilo(key=minted["token"])
        print(
            "the agent sees",
            len((await agent.workspace(workspace).list_resources())["rows"]),
            "apps",
        )

        # Revoking it ends the agent's access at once.
        await run(tilo, {"path": f"account/keys/{minted['grant']}", "action": "revoke"})
        try:
            await agent.workspace(workspace).list_resources()
            print("still allowed")
        except Refused as error:
            print("after revoking:", error.code, "at", error.error.get("revoked_at_ms"))
        await agent.aclose()


asyncio.run(main())
```

```typescript example=key-scopes
import { Refused, Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(tilo, { path: "account/workspaces", action: "create" }, { name: "One customer" });
const workspace = (made.result as { workspace: { id: string } }).workspace.id;

// A key that reaches this workspace and nothing else.
const mintedAnswer = await run(
  tilo,
  { path: "account/keys", action: "mint" },
  { workspace, name: "Their agent" },
);
const minted = mintedAnswer.result as {
  grant: string;
  token: string;
  workspace?: string;
  mcp_url?: string;
};
console.log("agent's MCP URL:", minted.mcp_url);
console.log("it reaches:", minted.workspace);

// The agent's own client: with that key it sees this workspace's apps.
const agent = new Tilo({ key: minted.token });
const apps = await agent.workspace(workspace).listResources();
console.log("the agent sees", apps.rows.length, "apps");

// Revoking it ends the agent's access at once.
await run(tilo, { path: `account/keys/${minted.grant}`, action: "revoke" });
try {
  await agent.workspace(workspace).listResources();
  console.log("still allowed");
} catch (error) {
  if (error instanceof Refused) {
    console.log("after revoking:", error.code, "at", error.error.revoked_at_ms);
  } else {
    throw error;
  }
}
agent.close();
```

```curl example=key-scopes
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "One customer"}' | jq -r .result.workspace.id)

# A key that reaches this workspace and nothing else.
minted=$(curl -sS --fail-with-body "$api/resources/account/keys/actions/mint" -H "$auth" -H "Content-Type: application/json" \
  -d "{\"workspace\": \"$workspace\", \"name\": \"Their agent\"}" | jq -r .result)
echo "agent's MCP URL: $(jq -r .mcp_url <<<"$minted")"
agent="Authorization: Bearer $(jq -r .token <<<"$minted")"

# With that key, the agent sees this workspace's apps.
apps=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources" -H "$agent" | jq '.rows | length')
echo "the agent sees $apps apps"

# Revoking it ends the agent's access at once.
curl -sS --fail-with-body "$api/resources/account/keys/$(jq -r .grant <<<"$minted")/actions/revoke" \
  -H "$auth" -H "Content-Type: application/json" -d '{}' > /dev/null
curl -sS "$api/workspaces/$workspace/resources" -H "$agent" \
  | jq -r '"after revoking: \(.error.code) at \(.error.revoked_at_ms)"'
```

## Where keys live

- **Your grants page**, `https://mcp.tilo.sh/grants`: every key and every agent you let in, with its
  scope, when it was last used, which of your devices approved it, and a Revoke button.
- **The command**: `tilo do account/keys#create name="My backend"` (with `--out <file>` it writes the
  key there instead), `tilo ls account/keys`, `tilo do account/keys#mint workspace=<workspace>`, and
  `tilo do "account/keys/<grant>#revoke"`.
- **The SDKs**: `run(tilo, {"path": "account/keys", "action": "list"})`, `#create`, `#mint` (with the
  workspace it reaches), and `"account/keys/<grant>"#revoke`.
