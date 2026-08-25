# The agent action API

qOS apps export typed actions. A program that cannot see the screen can ask the
shell what is possible right now, and then perform one of those things by name.
No screenshot, no hit-test, no synthetic click.

Two ops on the `QOS_TEST_CONTROL` socket carry this. Both use the existing
newline-delimited JSON framing described in `docs/headless-testing.md`: one JSON
object per line, request and response.

## `actions_enumerate`

```json
{"op":"actions_enumerate","query":"terminal"}
```

```json
{"answer":{"query":"terminal","count":2,"actions":[
  {"provider":"actions","id":"window.new.terminal","kind":"Actions",
   "title":"New terminal window","subtitle":null,"score":0}
]}}
```

The providers on a stock build are `actions` (the shell's own chrome),
`applications`, `host-applications`, `notes`, `files`, `records`, `editor` —
and `app`, which is the applications themselves.

An empty `query` lists every provider's default set rather than guessing a
needle. The descriptors come from the same provider registry that backs the
Ctrl+K command palette — the palette renders these as rows, enumeration renders
nothing.

## The `app` provider: reaching inside a running application

Everything above is the shell's own surface. `app` is not: its rows come from
the applications running on the desktop, each of which publishes what it has
open and what may be done to it over the socket the session handed it
(`publishActions`, in `crates/runtime/runtime-collab/schema/collab.capnp`).

```json
{"op":"actions_enumerate","query":"table"}
```

```json
{"answer":{"query":"table","count":1,"actions":[
  {"provider":"app","id":"word-processor.document#insert-table","kind":"App action",
   "title":"Insert table","subtitle":"Document — word_processor.document","score":0}
]}}
```

An address is `<resource id>#<action id>`, both minted by the app. A resource id
is app-namespaced by convention (`word-processor.document`,
`files.entry:/var/workspace/plan.md`), which is what lets the capability gate
name the app being acted on without the shell keeping a second table of who owns
what. Nothing in it is positional, so the same address keeps naming the same
operation across layouts, themes and resolutions.

### The app is what performs it

Invoking one of these does not run anything in the compositor. The address goes
back down the same socket to the one connection that published it, and the
application performs its own operation on its own document; the change then
reaches everyone else the way every other change does. It has to be that way
round — the live resource tree, the app's action decoders and the app's document
are all things only the app holds, and an `ActionSpec` for a prompted action
holds a closure that cannot cross a process boundary at all.

So `actions_invoke` waits for the application to say what became of it, bounded
at two seconds. Three outcomes, and they are deliberately three: it ran, it was
refused (with the app's own code and words), or nobody answered. A caller that
cannot see the screen has no other way to tell them apart.

### An app may only publish its own operations

The resource id is what the gate reads the app off, and it arrives *from* the
app — so on its own it would be a claim. The session drops any published
operation whose namespace is not the connection's own application, taken from
the route it joined on rather than from anything in the call. Without that, a
sandboxed app could publish `files.entry:/…#delete` and be gated as though it
were Files.

### An app that goes takes its operations with it

The publish is level-triggered: each one replaces that connection's whole
surface, so a resource that has closed stops being addressable rather than
lingering as an address that resolves to something gone. And when the RPC
session ends — however it ends — everything the connection published is
forgotten, and `ActionRegistry::forget_app` withdraws the declarations behind
it. An address that outlived the process behind it is worse than one that never
existed: it resolves, it is invoked, and nothing performs it.

## `actions_invoke`

```json
{"op":"actions_invoke","provider":"actions","id":"window.new.terminal"}
```

```json
{"answer":{"provider":"actions","id":"window.new.terminal",
           "kind":"Actions","title":"New terminal window"}}
```

An address that is unknown, or hidden from the caller, is refused — and the two
are deliberately indistinguishable, so probing cannot map what you may not see:

```json
{"err":{"code":"unknown_action",
        "message":"no visible action addressed by (actions, nope) in the current catalogue"}}
```

## Why enumeration and invocation are separate steps

They answer different questions and carry different risk. "What can I do here"
must be free to ask; "do this" must not be.

Collapsing them into one call — a search that runs its best match — makes every
query a potential side effect, so a caller exploring the surface cannot explore
it safely. Keeping them apart means a caller can inspect what an address
resolves to, reason about it, and only then commit. It also makes the failure
modes distinguishable: "that action does not exist for you" and "I ran it and
nothing happened" must not look alike to a caller with no way to check the
screen.

Invocation re-resolves the address against a freshly built catalogue before
anything runs, so an address that was valid at enumeration time and is no longer
valid now fails loudly instead of running something else.

## Why enumeration carries no invoke handle

The descriptors are inert data: `provider`, `id`, `kind`, `title`, `subtitle`,
`score`. There is no callback, no token, no handle. Read-only is a property of
the reply's shape, not a promise in a comment — there is nothing in an
enumeration result that could be fired, so surveying the surface cannot cause an
effect even by mistake.

An action is therefore addressed by a stable `(provider, id)` pair and never by
a screen position. That is what makes the surface drivable by something that
cannot see, and what makes an invocation reproducible across window layouts,
resolutions and themes.

## The visibility gate

`ActionCatalog` (`shell/src/shell/search/action_api.rs`) applies an
`ActionVisibility` gate when it is built, and keeps the result as an index list
of the entries that passed. Every public accessor — `enumerate`, `select`,
`invoke` — reads through that same list.

The consequence is the important part: a denied action is *absent*, not
*disabled*. It does not appear in enumeration, and `select` on its address
returns nothing, so `actions_invoke` refuses with `unknown_action`. A caller
cannot invoke what it was never shown, and cannot learn that a hidden action
exists by trying to run it. Contrast the usual arrangement, where a filtered
list and an unfiltered dispatcher disagree and the list is merely advisory.

The implementation the socket passes is `CapabilityVisibility`
(`shell/src/shell/search/visibility.rs`), which resolves the active workspace,
builds the actor for the local user, and asks that workspace's own capability
model. It is fail-closed twice over: `decide_capability` denies unless something
explicitly permits, and a session that cannot resolve a workspace at all errors
rather than falling back to showing everything. A gate that opens when it cannot
find its policy is not a gate.

### What is gated, and what is not

The rule is the one `shell/src/shell/actions.rs` already states, not a new one:
showing a resource is non-destructive and needs no permission; taking an action
on one requires permission. So notes, records, files and folders are never
hidden, and neither are the actions that merely open an overlay —
`settings.open` reveals a panel, it does not change a setting. What is gated is
acting: `Write` for spawning a terminal, duplicating a tab or launching an app,
`Invite` for peer operations. An action no mapping covers is treated as an act
requiring `Write`, so a provider that lands without one goes dark for a
restricted caller instead of being quietly reachable.

The effect is observable on a live session. Against a workspace the local user
owns, `actions_enumerate` returns the full surface — 191 entries on a stock
build. Against a workspace owned by someone else, the same call returns only
what anyone may see: the overlay-opening actions and the documents. No error, no
empty list, no hint that the rest exists.

## Pointing Claude Code at it

`scripts/qos-mcp-server.py` is an MCP stdio server exposing three tools —
`qos_enumerate_actions`, `qos_invoke_action`, `qos_state_dump`. Standard library
only, no SDK.

Start a compositor, then register the shim:

```bash
export QOS_TEST_CONTROL=/tmp/nirios.sock
cargo run --bin shell -- --backend headless &

# Verify the shim reaches the compositor before putting a model behind it:
scripts/qos-mcp-server.py --selftest

claude mcp add qos -- /absolute/path/to/scripts/qos-mcp-server.py
```

The socket path comes from `$QOS_TEST_CONTROL`, or `--socket`. The shim never
fabricates an action list or a success: if the compositor is unreachable, or
answers `{"err": ...}`, the model receives that as a tool error with the
compositor's own code and message intact.

## Source of truth

| What | Where |
|---|---|
| Command + response shapes | `shell/src/compositor/test_control.rs` |
| Live enumerate / resolve | `shell/src/compositor/native.rs` |
| Catalogue + visibility seam | `shell/src/shell/search/action_api.rs` |
| Capability gate + policy | `shell/src/shell/search/visibility.rs` |
| App wire (schema) | `crates/runtime/runtime-collab/schema/collab.capnp` |
| App wire (session side) | `shell/src/shell/app_action_wire.rs` |
| App wire (app side) | `ui::App::publish_action_surface`, `ui/src/app.rs` |
| App provider | `shell/src/shell/search/providers/app_resources.rs` |
| Live end-to-end proof | `scripts/headless-examples/action-api-demo.py` |
| Live proof across a process boundary | `scripts/headless-examples/app-action-wire.py` |
| MCP shim | `scripts/qos-mcp-server.py` |
| Socket framing, other ops | `docs/headless-testing.md` |
