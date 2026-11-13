# Tilo docs

Tilo gives each of your customers, or each of your own jobs, a workspace of its own: apps a person can
open, records your code can read and change, and keys that decide who reaches what. Your agent works
in it, your backend scripts it, and a person checks the result in the same place.

This page is the map. The [quickstart](quickstart.md) gets you from nothing to a first call.

## What you get

- **A workspace per customer, or per job.** Its own apps and its own files. Nothing is shared with
  another workspace unless you put a person in both.
- **Apps people already know how to use.** Notes, Writer, PDF Viewer, a Browser, a Terminal, Mail. The
  same apps run in the desktop app and in the browser at [tilo.sh](https://tilo.sh).
- **Everything addressable.** An app's resources have paths, its actions are named, and the fields are
  typed. An agent, a script and a click run the same action ([resources and actions](resources-and-actions.md)).
- **History you can audit and undo.** Each change says who made it and which run it belonged to, and a
  whole run can be taken back in one go ([runs and history](runs-and-history.md)).
- **A person in the loop where it matters.** An action that leaves the workspace, such as submitting a
  web form, waits for a person to allow it ([approvals](approvals.md)).

## Three ways in

| Who | What they use | Where it goes |
| :--- | :--- | :--- |
| Agents | MCP, one line in your agent's config | `https://mcp.tilo.sh` for every workspace, `https://mcp.tilo.sh/w/<workspace>/mcp` for one |
| Your code | the Python and TypeScript SDKs, the `tilo` command, tilolang | the same addresses, with typed answers |
| People | Tilo in the browser at [tilo.sh](https://tilo.sh), or the Tilo desktop app | the workspace's apps, live |

All three reach the same resources through the same actions, under the same permissions. An agent does
not have a private back door, and your script does not have a second API to keep in step.

## A key is the whole story of access

You approve a key once in your Tilo, and it names what it reaches: every workspace of yours, or one
workspace. A key that reaches one workspace is what you hand a customer's agent, and it cannot see,
list or act in any other. Keys expire, can be revoked, and can mint narrower keys; revoking a key ends
every key minted from it ([identities and keys](identities.md#keys)).

## Where to start

- **[Quickstart](quickstart.md)**: sign in, make a workspace, connect your agent, make your first call.
- **[One workspace per customer](../developers/one-workspace-per-customer.md)**: the shape most backends want.
- **[The hosted Browser](hosted-browser.md)**: an agent fills a web form, a person approves the
  submission.
