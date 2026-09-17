# Native shared-runtime MCP host

`qos-runtime-mcp` exposes the action directories of runtime-managed qOS apps on
one newline-delimited MCP stdio connection. It starts the production identity
and collaboration brokers through `runtime_host::Session`, then launches the
user-selected executables with the same inherited service descriptors as the
shared-runtime proof. Individual apps contain no MCP registration hook.

Build the host on macOS or Linux:

```sh
cargo build -p shell --bin qos-runtime-mcp --no-default-features
```

Choose a short absolute profile path and create an app manifest at an absolute
path. Unix socket pathname limits also apply to the profile’s `collab.sock` and
`identity.sock`; long profile paths fail at startup. For example, use a private
`/tmp/qos-mcp-...` directory for temporary runs. The executable paths must also
be absolute. The list may be empty, and repeated
app ids intentionally create separate live instances:

```json
{
  "apps": [
    {
      "id": "dev.qos.photos",
      "executable": "/absolute/path/to/photos",
      "args": ["--open", "/absolute/path/to/source.png"]
    },
    {
      "id": "dev.qos.word-processor",
      "executable": "/absolute/path/to/word-processor",
      "args": []
    }
  ]
}
```

One MCP client configuration shape is:

```json
{
  "mcpServers": {
    "qos-runtime": {
      "command": "/absolute/path/to/qos-runtime-mcp",
      "args": [
        "--profile", "/absolute/path/to/persistent-profile",
        "--apps", "/absolute/path/to/apps.json"
      ]
    }
  }
}
```

The profile holds workspace configuration, identity material, app data, MLS
state, cache, shared blobs and per-instance app logs. Only one host may own a
profile at a time. Contention is reported as an error; the host does not remove
another process's lock or sockets. EOF, output failure, app launch failure, or
an unexpected app exit ends the connection and reaps only children started by
that host. App stdin is null and app stdout/stderr go to instance-indexed log
files, so only JSON-RPC replies appear on MCP stdout. Request lines are limited
to 1 MiB and the pending input queue is bounded.

## Discovery and invocation

The endpoint implements `initialize`, `ping`, `tools/list`, `tools/call`,
`resources/list`, and `resources/read`. Its generic tools list live app
instances, browse a bounded lazy resource/action directory, resolve an exact
current action, start it, inspect its canonical status, and request
cancellation. Call `qos_list_apps`, then `qos_query`; preserve the returned peer
id, revision, resource and action for `qos_resolve` or `qos_invoke`. Browse
roots with an empty `parent_resource`, then use a returned resource as
`parent_resource` to discover its actions and children. Follow `next_cursor`
with the same revision and query. The optional `needle` searches declared
search providers; it is not a search across every action label. After an
invocation is accepted, poll `qos_status` until its real terminal update. Pass
content resource references returned by one successful action as typed
arguments to a freshly discovered action in another app. The runtime contains
no Photos/Writer workflow branch and never treats `Accepted` as `Succeeded` or
replays a mutation automatically.

App publication is automatic only for apps launched from this manifest. A
normal `ui::App` connects through its inherited collaboration and identity
descriptors and publishes its current lazy directory. An app started directly
from Finder, Cargo, or another process manager has no descriptors for this
session and does not attach automatically.

## Authority and scope

This is a local, same-user native host. The profile's workspace and actor are
fixed when the host starts; MCP requests cannot select an actor, workspace, or
executable. Discovery and invocation retain the production broker's workspace
authority and current-instance checks. Actions requiring confirmation are
refused because this stdio host has no real approval ceremony; a caller boolean
cannot manufacture broker admission.

The concrete launcher uses Unix descriptor inheritance and is currently for
macOS and Linux. Windows host transport and process inheritance remain
unqualified. The repository-wide source audit found that all 47 inventoried GUI
entry points reach the shared production `ui::App` publication path, including
six custom live handlers. That is source coverage, not native execution proof
for all 47 apps. The headless Agent is a consumer rather than a GUI publisher.

Building or running this binary does not install or configure an MCP client,
change an account, enable a cloud relay, or add remote federation. Client setup
and the app manifest remain explicit user configuration.
