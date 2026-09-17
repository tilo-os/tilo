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

## Native acceptance proof

Build the host, three apps and test-only evidence inspector, then run:

```sh
cargo build -p shell --bin qos-runtime-mcp --example runtime-mcp-inspect \
  -p photos --bin photos -p word-processor --bin word-processor \
  -p calculator --bin calculator --no-default-features
python3 scripts/runtime-mcp-proof.py --output /absolute/path/to/new-evidence
```

This opens real Photos, Writer and Calculator windows. The runner copies the
executables into the evidence directory and records their SHA256 hashes before
and after execution. It uses a separate short private temporary profile for the
Unix sockets, retaining logs and public workspace evidence without copying the
profile's private identity material.

Every crop, export, image insertion and Calculator mutation is an external MCP
`tools/call`. Discovery is refreshed between operations, and accepted invocations
are observed to a terminal result without replay. Read-only discovery may retry
an explicitly stale revision. The fixture is a patterned 48×32 PNG; the centered
square crop is checked against source pixels `(x + 8, y)` in every RGBA channel.
The exported BLAKE3 content resource is converted using `qos_actions::content_hash`
and passed to Writer's freshly discovered `place-image` action.

A test-only observer is launched through the same manifest with Writer's app
identity and ordinary authenticated SDK subscription. It publishes no directory
and sends no document or action mutation. A bounded readiness wrapper starts
real Writer after subscription; the inspector independently checks the signed
envelopes' signatures and exact app/workspace/empty-record route, reconstructs
Automerge, and verifies the image block's exported hash and dimensions. Native
untitled Writer does not have a persistent record id, so the proof does not
invent one or bypass the confirmation required for `save-as`.

The proof also invokes Calculator's declared `add-function` operation and
checks fresh graph-resource discovery. Negative evidence distinguishes malformed
typed arguments, an explicitly stale revision, and an unavailable instance id;
the last is not a previously connected peer-disconnect test. Retained evidence
includes the transcript, terminal outputs, source/result images, signed events,
reconstructed document, logs, binary provenance and exact owned-PID cleanup.
`proof.json` appears only after all checks and shutdown succeed; failed attempts
retain `failure.txt` instead. This is three-app macOS behavioral evidence when
run successfully, separate from the 47-app source audit, and does not qualify
Windows or install an external ChatGPT/Claude client.

Verified on macOS on 2026-09-17: the retained run at
`target/runtime-proof/mcp-native9/proof.json` passed crop → export → Writer,
checked all 1,024 result pixels, and reconstructed the Writer image block from
two verified signed envelopes. The actual exported BLAKE3 hash was
`9efff46c8f9248809cb22f7ad61bcb71bf5fa54995d2e67af8342f70aceb739e`;
Writer referenced that same hash with width and height 32. Calculator's fresh
`calculator.graph` resource contained `sin(x) + 0.314159`. All four workflow
invocations reached `succeeded`; malformed arguments reached `failed`, and the
stale revision and unavailable instance were refused. The host exited zero,
all five known process IDs were absent after cleanup, and retained binary hashes
were unchanged. The Mac was locked: this verifies background native behavior,
content and signed document state, not visible presentation or frame rate.
Earlier failed attempts remain alongside this run and are not passing evidence.
