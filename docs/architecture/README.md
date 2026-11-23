# Architecture

Tilo is a desktop, a set of apps and the system underneath them, built so that
people and their devices work on the same things together. This page is the
map: the model every part follows, and where each part lives in the code.
[`workspace.md`](workspace.md) is the full specification of the model; the
[code map](../codemap.md) lists every crate. [`flows.md`](flows.md) lists the uses of
Tilo that must work end to end.

## The model

**A workspace is the one unit of collaboration.** It holds a layout (strips,
window groups and the apps open in them), people (members, roles and grants)
and state with its history. Every workspace a person has is on every one of
their devices. There is nothing beside or under a workspace.

**A workspace has one set of resources.** Its files, mail, calendar, vault,
models, connections, agents and skills belong to the workspace, and every member
uses them as the permissions set by the workspace's owners allow. Only what
belongs to a person's identity is theirs alone: their keys and devices,
appearance and device settings.

**One identity system, and your identity controls your devices.** The OS, the
web and the CLI use the same identity. A member is a person's identity; devices
are separate. A member is a
certified identity root. Each of their devices has its own key, certified by
that root, and that key is the device's iroh endpoint id. Membership, roles,
grants and history name members, never devices. In each workspace a device is
either a *key holder* (holds the keys, reads, writes and can do work) or a
*store* (keeps and forwards the encrypted data and can read none of it), and
each device decides how much it offers: storage, sync, running actions,
search, agents. A server is a device with a large appetite, not a different
kind of node: per workspace it stores ciphertext or holds the keys and takes
work, and it acts for a member only under that member's delegation. A
workspace held by one person's PC is complete; servers add reachability,
copies and compute, never a requirement. Tilo works with no server at all;
tilo.sh is our cloud offering of such devices for people who want one.
Your identity certifies, revokes and configures each of your devices, and every
device it certifies gets your workspaces. It is also your login at tilo.sh;
there is no separate account. The accounts a person already has (mail, cloud
storage, calendars and others) connect to a workspace, to bring their data
over or to keep using them there.

**Devices sync over iroh, end to end encrypted.** Devices reach the other
devices of a workspace directly or through any device that stores it. Content
is sealed with MLS content keys. Shared documents can use the workspace group.
A restricted document has its own group, which gives history keys only to
devices currently permitted to read it.

**All shared state is CRDT.** Every resource is an Automerge document,
including settings. Membership itself is a resource (`workspace.members`)
whose ops are signed and checked against the membership as their author saw
it, so every key holder reaches the same answer without a coordinator.

**Apps are typed resources, actions and bound views,** in tilolang or in Rust;
both use the same model. An app declares its
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

**These are the default, at every level of customization.** Apps climb a
ladder from a tilolang view to fully hand-written Rust. Each step allows more
customization, and every step keeps the list above, views that can be shown
elsewhere, coherent UX and good animation. There is no ceiling: a Minecraft
launcher or a CAD program is a Tilo app too.

**The default apps are real.** Files, Writer, Ebooks, Settings, a video editor
and the rest are enough for 95% of people, not proofs of concept. Their UX is
polished: Settings, Writer, the video editor and the rest have custom
interactive animations and visualizations on their pages. None of this is
optional.

**Tilo feels immediate.** An app opens in under 100 ms when it's already on
the device, and under 500 ms when it first has to be installed. Apps are
small, and the web version loads about 1 MB of WASM and HTML.

**Every actor uses the same actions, and every action can be seen.** People,
agents, scripts and automations invoke the same typed actions. An action belongs
to a resource and its views, so when an agent calls one, every view showing that
resource shows the change and who made it, in whichever shell view the person
uses.

**Views compose, and anyone can write one.** A view presents resources and
places their actions as controls. One view can contain another, and a resource
can be shown again anywhere: in another app, in the shell, in a chat. A person
can change a view, or ask an agent in a chat to write a new one in tilolang,
without rebuilding Tilo.

**The shell is a view, on the same stack.** A person can customize it as deeply
as any other view, or replace it: a shell can look like Windows 95 or work
like a Miro or Figma canvas, and tilolang is powerful enough to write either. The launcher, top bar, overview, quick
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
| Resources, actions and their macros | `crates/core/tilo-resource`, `crates/core/tilo-resource-macros` |
| UI toolkit: widgets, layout, text, rendering, headless testing | `crates/ui/ui-core` |
| History, search | `crates/sdk/tilo-history`, `crates/engines/search` |
| Example apps, one idea each | `examples/apps` |
| First-party apps | `apps/` ([list](../../apps/README.md)) |
| Identity, MLS groups, action policy, collaboration state | `crates/core/` (`tilo-user-identity`, `tilo-device-identity`, `key-group`, `action-policy`, `tilo-peer-state`) |
| Automerge (vendored fork) | `vendor/automerge`, used through `crates/core/tilo-automerge` |
| Files and storage | `crates/core/fs`, `crates/core/fs-native` |
| Compositor and shell | `crates/desktop/session` |
| Web runtime (the page a browser enters a workspace with) | `crates/core/tilo-browser-runtime` |
| Platform services with one API on Linux, macOS and the web | `crates/platform/` |
| Packages, sandboxed launch, the registry | `crates/core/` (`launcher`, `package-service`, `package-store`, `app-registry`, `registry-server`) |
| The `tilo-app` command | `tools/tilo-app-cli` |
| The OS image | `os/` |

Apps and the portable SDK crates hold no platform code: anything that differs
per target goes through a crate in `crates/platform/`. The policy suite
(`tools/policy/run.sh`) enforces this and the other repository rules listed in
[policies](../policies.md).

## Where the code still differs

[The core](core.md) is the specification of the six core pieces (devices,
identity, sync, the app framework, the workspace and actions everywhere): how
each works on `main`, every copy of it, the one design that survives and what
is deleted.

## More

- [The core](core.md): the six core pieces, their copies and what survives.
- [Workspaces](workspace.md): the specification.
- [Resources](resources.md): resource kinds, ids and documents.
- [Compatibility](compatibility.md): how versions of formats and interfaces
  stay readable.
- [Security model](../security/README.md): threat model, sign-in, boot chain.
- [Surfaces](surfaces.md), [window management](window-management.md),
  [theming](theming.md): the shell and the toolkit.
- [Several actors in one web page](web-page-actors.md): agents, people and
  devices working in one Browser tab, the tab's record, review, and a form as
  typed actions.
- [Session UI inventory](ui-inventory.md): which shell surfaces still lay
  themselves out by hand, and what replaces each.
