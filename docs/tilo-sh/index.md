# Tilo docs

Tilo gives you shared workspaces for your devices, the people you work with, and your AI agents.
Every workspace has its own apps, files, permissions, and history. Open it in the browser at
[tilo.sh](https://tilo.sh), on Tilo OS, or drive it from code and MCP.

Start with the [quickstart](quickstart.md) to make a workspace, connect an agent, and invoke your
first action.

## What you get

- **Isolated workspaces:** Each workspace has its own members, apps, files, and permissions. Nothing
  leaks across workspaces unless you explicitly share it.
- **Real apps, live in one place:** Notes, Writer, PDF Viewer, Browser, Terminal, Mail, and any
  [tilolang](https://tilolang.dev) app or custom view work identically on Tilo OS and in the
  browser at [tilo.sh](https://tilo.sh).
- **One typed interface for people, scripts, and agents:** Every app exposes typed resources and
  named actions. A click, a CLI command, a Python or TypeScript SDK call, and an agent MCP call all
  invoke the exact same action ([resources and actions](resources-and-actions.md)).
- **Full history and per-author undo:** Every change tracks who made it and which invocation it
  belonged to, and any step or whole invocation can be undone
  ([invocations and history](invocations-and-history.md)).
- **Approvals where it matters:** Actions that leave the workspace, such as submitting a web form,
  pause for a person to approve before proceeding ([approvals](approvals.md)).

## Three ways in

| Who | What they use | Where it goes |
| :--- | :--- | :--- |
| Agents | MCP, one line in your agent's config | `https://mcp.tilo.sh` for all your workspaces, `https://mcp.tilo.sh/w/<workspace>/mcp` for one |
| Your code | Python & TypeScript SDKs, the `tilo` CLI, or tilolang | the same addresses, with typed answers |
| People | Tilo in the browser at [tilo.sh](https://tilo.sh), or Tilo OS | the workspace's apps, live |

All three reach the same resources through the same actions, under the same permissions. Agents
never use a separate path, and scripts never maintain a second API.

## Scoped keys

You approve a key once in Tilo and choose what it reaches: all of your workspaces, or one specific
workspace. Keys can expire, be revoked at any time, or mint narrower keys; revoking a parent key
revokes every key minted from it ([identities and keys](identities.md#keys)).

## Where to start

- **[Quickstart](quickstart.md):** sign in, make a workspace, connect your agent, and make your
  first call.
- **[Workspaces](workspaces.md):** roles, inviting people and agents, and branching workspaces.
- **[The hosted Browser](hosted-browser.md):** an agent fills a web form while a person approves
  the submission.
