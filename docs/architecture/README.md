# Architecture

Tilo is a desktop, a set of apps and the system underneath them, built so that
people and their devices work on the same things together. This page is the
map: the model every part follows, and where each part lives in the code.
[`workspace.md`](workspace.md) is the full specification of the model; the
[code map](../codemap.md) lists every crate.

## The model

**A workspace is the one unit of collaboration.** It holds a layout (strips,
window groups and the apps open in them), people (members, roles and grants)
and state with its history. Every workspace a person has is on every one of
their devices. There is nothing beside or under a workspace.

**A member is a person's identity; devices are separate.** A member is a
certified identity root. Each of their devices has its own key, certified by
that root, and that key is the device's iroh endpoint id. Membership, roles,
grants and history name members, never devices. In each workspace a device is
either a *participant* (holds the keys, reads, writes and can do work) or a
*store* (keeps and forwards the encrypted data and can read none of it), and
each device decides how much it offers: storage, sync, running actions,
search, agents. A "host" is a device with a large appetite, not a different
kind of node.

**Devices sync over iroh, end to end encrypted.** Devices reach the other
devices of a workspace directly or through any device that stores it. Content
is sealed under content keys whose group is an MLS group made of exactly the
workspace's participant devices. Members added later receive the keys of every
past epoch, so they read the whole history.

**All shared state is CRDT.** Every resource is an Automerge document,
including settings. Membership itself is a resource (`workspace.members`)
whose ops are signed and checked against the membership as their author saw
it, so every participant reaches the same answer without a coordinator.

**Apps are typed resources, actions and bound views.** An app declares its
state as a model (`#[derive(Model)]`), the changes it allows as actions
(`#[resource]` with `#[action]` methods), and views whose controls bind to
fields and actions. Because every change is a typed action on a CRDT resource,
the framework gives every app the same things for free:

- **collaboration and sync**: actions become CRDT changes that merge across
  people and devices;
- **history and undo**: every action is a history entry with an author, and
  undo is per author;
- **permissions**: roles and grants decide who may invoke which action, and a
  refused action changes nothing;
- **search**: resources are indexed by the system, so system search finds
  them;
- **scripts, agents and the command palette**: they enumerate and invoke the
  same actions the buttons do.

[Build an app](../app-development.md) shows this in code.

**The shell runs on the same stack.** The launcher, top bar, overview, quick
settings, notifications and onboarding are apps on the same toolkit and the
same resource/action/view model. The compositor stays thin: windows, input,
outputs and scanout. Rendering capabilities the shell needs belong in the
toolkit, so every app has them.

## Where the code is

Tilo is one Rust workspace. The root `Cargo.toml` lists every member; package
names are stable even when directories move.

| Part | Where |
| --- | --- |
| App SDK: the public facade apps depend on (as `ui`) | `crates/sdk/tilo-app` |
| Resources, actions and their macros | `crates/sdk/tilo-resource`, `crates/sdk/tilo-resource-macros` |
| UI toolkit: widgets, layout, text, rendering, headless testing | `crates/sdk/ui-core` |
| History, search | `crates/sdk/tilo-history`, `crates/sdk/search` |
| Example apps, one idea each | `examples/apps` |
| First-party apps | `apps/` ([list](../../apps/README.md)) |
| Identity, MLS channels, action policy, collaboration state | `crates/collab/` (`tilo-user-identity`, `channel-crypto`, `action-policy`, `tilo-peer-state`) |
| Automerge (vendored fork) | `vendor/automerge`, used through `crates/doc/tilo-automerge` |
| Files and storage | `crates/system/fs`, `crates/system/fs-native` |
| Compositor and shell | `crates/session/shell` |
| Web runtime and web desktop | `crates/browser/tilo-browser-runtime`, `crates/browser/tilo-web-desktop` |
| Platform services with one API on Linux, macOS and the web | `crates/platform/` |
| Packages, sandboxed launch, the registry | `crates/packaging/` |
| The `tilo-app` command | `crates/dev/tilo-app-cli` |
| The OS image | `os/` |

Apps and the portable SDK crates hold no platform code: anything that differs
per target goes through a crate in `crates/platform/`. The policy suite
(`tools/policy/run.sh`) enforces this and the other repository rules listed in
[policies](../policies.md).

## Where the code still differs

The code still carries models this page does not have: rooms beside
workspaces, governance by one device, per-person spaces, settings outside the
resource model, and a second platform inside the shell. Each is being replaced
by the model above, not kept beside it. [`workspace.md`](workspace.md), "Where
the code differs", lists them.

## More

- [Workspaces](workspace.md): the specification.
- [Resources](resources.md): resource kinds, ids and documents.
- [Compatibility](compatibility.md): how versions of formats and interfaces
  stay readable.
- [Security model](../security/README.md): threat model, sign-in, boot chain.
- [Surfaces](surfaces.md), [window management](window-management.md),
  [theming](theming.md): the shell and the toolkit.
