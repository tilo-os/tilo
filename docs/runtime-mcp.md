# Native shared-runtime MCP host

`qos-runtime-mcp` exposes the action directories of native qOS apps on one
newline-delimited MCP stdio connection. A normal framework GUI launch joins the
same per-user runtime automatically. The first app starts the background daemon;
later apps and MCP clients reuse it. Individual apps need no manifest, service
descriptor setup, MCP registration hook, or app-specific routing.

Build the host on macOS or Linux:

```sh
cargo build -p shell --bin qos-runtime-mcp --no-default-features
```

In an installed layout, app startup finds the sibling `qos-runtime-mcp` binary,
then falls back to `PATH`. In a source checkout, the first ordinary `cargo run`
can bootstrap a missing host with a bounded, serialized Cargo build using the
checkout's normal Cargo cache, network, target directory, and explicit offline
settings. Set `QOS_RUNTIME_BINARY` only when packaging or testing must select a
specific absolute host binary. Startup errors are printed instead of silently
running an app disconnected.

Without configuration, the persistent profile is
`$HOME/Library/Application Support/qOS/runtime` on macOS and
`$HOME/.local/state/qos/runtime` on other Unix systems. An absolute
`QOS_RUNTIME_PROFILE` selects another profile, which is useful for isolated
development runs. The daemon owns the profile independently of every app and
MCP client: closing one app or one MCP connection does not stop the daemon or
the other apps. Only one daemon may own a profile, and private-directory and
socket checks reject unsafe or competing owners.

## Assistant connection

Automatic app joining does not install or silently configure an assistant.
Point the assistant at the existing host with `--connect` and the same profile:

```json
{
  "mcpServers": {
    "qos-runtime": {
      "command": "/absolute/path/to/qos-runtime-mcp",
      "args": ["--profile", "/absolute/path/to/persistent-profile", "--connect"]
    }
  }
}
```

The first ordinary app may start the daemon before the assistant connects. A
`--connect` client can also start it when no app is running. Stdio EOF ends only
that proxy connection.

## Explicit manifest mode

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

The original explicit launcher remains available for controlled workflows:

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

Manifest mode owns the processes it launches. EOF, output failure, app launch
failure, or an unexpected child exit ends that manifest session and reaps only
its children. App stdin is null and app stdout/stderr go to instance-indexed
logs, so only JSON-RPC replies appear on MCP stdout. Request lines are limited
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

A normal `ui::App` automatically obtains collaboration and identity
connections from the shared runtime and publishes its current lazy directory.
Existing qOS or legacy inherited connections remain authoritative and are not
replaced by another profile. Complete malformed inherited transfers fail
closed. Diagnostic, audit, shot, headless, and handled-command launches remain
detached, and an app may explicitly opt a private/local mode out before live
construction.

## Authority and scope

This is a local, same-user native host. The profile's workspace and actor are
fixed when the host starts; MCP requests cannot select an actor, workspace, or
executable. Discovery and invocation retain the production broker's workspace
authority and current-instance checks. Actions requiring confirmation are
refused because this stdio host has no real approval ceremony; a caller boolean
cannot manufacture broker admission.

The concrete launcher uses Unix sockets, process execution, and descriptor
inheritance and is currently qualified for macOS and Linux. Windows host
transport and inheritance remain unsupported. The repository-wide source audit
found that all 47 inventoried GUI entry points reach the shared production
`ui::App` publication path, including custom live handlers. That source audit
is not native execution proof for all 47 apps. The headless Agent is a consumer
rather than a GUI publisher.

Building or running this binary does not configure an MCP client, change an
account, enable a cloud relay, or add remote federation. Assistant setup remains
explicit. `QOS_RUNTIME_STANDALONE=1` is a deliberate test/diagnostic opt-out
when no inherited qOS connection exists; it is not the ordinary default.

## Native acceptance proof

Capture the host inputs, build the host from those inputs, then run the
ordinary-launch proof in that order. Do not rebuild or edit the captured source
between these commands:

```sh
python3 scripts/runtime-auto-attach-proof.py \
  --capture-host-source-snapshot /absolute/path/to/host-build-source-hashes.json
cargo build -p shell --bin qos-runtime-mcp --no-default-features
python3 scripts/runtime-auto-attach-proof.py \
  --host-source-snapshot /absolute/path/to/host-build-source-hashes.json \
  --output /absolute/path/to/new-evidence
```

The runner opens Writer, Files, and Photos with literal ordinary `cargo run`
commands, waits for the first app to start the daemon, and only then opens an
explicit `--connect` proxy. Calculator starts late through its own ordinary
`cargo run`, demonstrating generic joining rather than a three-app allowlist.
The runner requires four distinct peer registrations, invokes Calculator's
declared `add-function` through MCP, and polls its canonical status to
`succeeded`. It then closes Writer and requires Files, Photos, Calculator, the
daemon, and MCP client to remain live.

Evidence includes exact commands, relevant source hashes before and after the
run, executable hashes, actual app/runtime/MCP PIDs, the complete MCP transcript,
terminal action output, app logs, and exact-PID cleanup. App data and runtime
authority use separate isolated directories. `proof.json` appears only after
the proxy exits cleanly, every known PID is absent, and the temporary runtime
profile has been removed. `cleanup.json` records profile-removal success or its
exact error; failed attempts retain `failure.txt` instead.

Verified on macOS on 2026-09-17 at
`target/runtime-proof/runtime-auto-attach/native-3/proof.json`: Writer, Files,
Photos, and late Calculator registered as peers 1–4 in daemon PID 28409;
Calculator invocation 1 reached `succeeded`; after Writer PID 28361 exited, the
other three registrations, daemon, and MCP PID 28548 remained live. Cleanup
confirmed all six known PIDs absent. The relevant source snapshot was unchanged
and exact binary hashes are retained with the run. This qualifies native shared
runtime behavior, not visible frames, performance, Photos History acceptance,
all 47 apps, or Windows.
