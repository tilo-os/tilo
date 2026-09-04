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
`applications`, `host-applications`, `notes`, `files`, `records` and `editor`.

An empty `query` lists every provider's default set rather than guessing a
needle. The descriptors come from the same provider registry that backs the
Ctrl+K command palette — the palette renders these as rows, enumeration renders
nothing.

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

`scripts/qos-mcp-server.py` is an MCP stdio server exposing the typed qOS
tools and a lazy resource tree. `resources/list` returns only application
surfaces at `qos://application/<app>`; it does not put every action schema into
the model's initial context. Reading one application returns the objects it
currently publishes with each object's full action descriptors nested beneath
it.

This nesting is the resource/action model rather than an MCP-only taxonomy:
an application owns resources (the object a person would recognise, such as a
file or document), and each resource owns the operations valid for it now.
Shell palette providers that do not publish a resource tree remain reachable
as one provider application with one invokable operation per object; the shim
does not infer a fictional hierarchy from dots in their display ids.

Every level re-enumerates through the visibility gate before answering. A
closed object or revoked action therefore disappears from its parent and a
stale URI fails as unknown; nesting changes when descriptors enter context,
not what the caller is allowed to reach. Standard library only, no SDK.

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

## Carrying one action result into the next

An action sequence is a data path, not just a list of commands. The broker's
terminal `Succeeded` update contains the declared `ActionOutput`; the agent
retains that output for exactly the immediately following step. Content bytes
are represented by one canonical resource address, `qos.content:<hash>`. An
action built with `prompted_content` consumes that hash from the preceding
application result and replaces any value the model tried to provide.

Local collaborative-document actions use the same rule through the owning
application's declared product function. The agent does not inspect a document
and guess which bytes it represents. Missing content, the wrong resource kind,
or more than one content result stops the sequence rather than selecting one.

This is deliberately not a workflow language: there are no named variables,
JSONPath expressions, graphs or implicit reuse of an older result. The narrow
one-step hand-off is enough to express a real cross-app operation such as
“rotate the photograph, then place the result in the document”, while keeping
the origin of the content mechanically checkable.

## Source of truth

| What | Where |
|---|---|
| Command + response shapes | `shell/src/compositor/test_control.rs` |
| Live enumerate / resolve | `shell/src/compositor/native.rs` |
| Catalogue + visibility seam | `shell/src/shell/search/action_api.rs` |
| Capability gate + policy | `shell/src/shell/search/visibility.rs` |
| Live end-to-end proof | `scripts/headless-examples/action-api-demo.py` |
| Cross-app result hand-off proof | `apps/agent/tests/discovered_operations.rs` |
| MCP shim | `scripts/qos-mcp-server.py` |
| Socket framing, other ops | `docs/headless-testing.md` |
