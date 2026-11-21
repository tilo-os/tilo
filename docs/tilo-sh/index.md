# Tilo Documentation

Tilo gives you shared workspaces for your devices, the people you work with, and your AI agents.
Every workspace has its own apps, files, permissions, and history. You can open it in the browser
at [tilo.sh](https://tilo.sh), on Tilo OS, or drive it from code and MCP.

Start with the [quickstart](quickstart.md) to create a workspace, connect an agent, and invoke your
first action.

## Core capabilities

- **Isolated workspaces:** Each workspace has its own members, apps, files, and permissions. Data
  never crosses between workspaces unless you share it.
- **Same apps on desktop and web:** Notes, Writer, PDF Viewer, Browser, Terminal, Mail, and any
  [tilolang](https://tilolang.dev) app or view work identically on Tilo OS and in the browser at
  [tilo.sh](https://tilo.sh).
- **One typed action API:** Every app exposes typed resources and named actions. UI clicks, CLI
  commands, Python/TypeScript SDK calls, and agent MCP calls all invoke the same actions
  ([resources and actions](resources-and-actions.md)).
- **History and per-author undo:** Every change tracks its author and invocation, and any step or
  invocation can be undone ([invocations and history](invocations-and-history.md)).
- **Approvals:** Actions that leave the workspace, such as submitting a web form, pause until a
  person approves them ([approvals](approvals.md)).

## Interfaces

| Client | Protocol / tool | Endpoint |
| :--- | :--- | :--- |
| **AI agents** | MCP | `https://mcp.tilo.sh` (all workspaces) or `https://mcp.tilo.sh/w/<workspace>/mcp` (single workspace) |
| **Scripts & backends** | Python & TypeScript SDKs, `tilo` CLI, or tilolang | Same workspace & resource paths, with typed responses |
| **People** | Web client at [tilo.sh](https://tilo.sh) or Tilo OS | Live interactive apps |

All three clients call the same actions under the same permission checks.

## API keys and scoping

You approve an API key once in Tilo and scope it either to all of your workspaces or to a single
workspace. Keys support expiration, immediate revocation, and minting narrower child keys; revoking
a parent key revokes every child key minted from it ([identities and keys](identities.md#keys)).

## Next steps

- **[Quickstart](quickstart.md):** sign in, create a workspace, connect an agent, and make your
  first SDK call.
- **[Workspaces](workspaces.md):** roles, invites, labels, and branching.
- **[Hosted Browser](hosted-browser.md):** agents and people sharing a browser tab with human
  approvals.
