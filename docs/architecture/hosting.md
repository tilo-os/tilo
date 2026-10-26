# Hosting: workspaces, hosts, devices and identity

This page is the target for how workspaces are hosted, reached and shared.
Some of it has no interface yet or comes later. The model must still allow
it without a rewrite, so every change to rooms, hosts, devices or identity is
checked against this page. The workspace itself, one room holding layout,
people, and state with its history, is defined in
[`workspace.md`](workspace.md). Rooms and records are in [`rooms.md`](rooms.md),
and transport and governance in [`collaboration.md`](collaboration.md).

## One workspace, reached many ways

A workspace made on any device is a workspace, the same kind everywhere.
Nothing about it depends on where it was made.

- **Invite.** Its people can invite others. An invite is a link.
- **Open on the web.** Someone with the link can open the workspace on
  tilo.coffee in a browser, with no install.
- **Add on a device.** Someone who opens the link on their own Tilo device
  adds the workspace to that device's workspace list, beside their own.
- **Peer to peer.** Devices and hosts connect directly over Iroh, the only
  network transport. A relay is used only when a direct path can't be found;
  it changes reachability, never who may do what.
- **Your workspaces from anywhere.** Signed in on tilo.coffee with their
  identity, a person can open any of their workspaces, wherever they are
  hosted. They can also create a new workspace on any of their devices.
  Creating needs a host that can take it: tilo.coffee hosts no one's
  workspaces at first, only the Lounge.

## Hosts

A host is anything that holds a workspace's room and records for others and
stays reachable: a person's own device, a home relay, or tilo.coffee.

### Governance and placement are separate

- **One governor per room.** Exactly one host is the room's governor. It
  admits people and devices, holds the room's grants, and decides what the
  room document accepts. Governance can be handed over, for example to
  another of the owner's devices or to a host added later. Handover keeps
  the room, its ids and its history; see governor succession in
  [`collaboration.md`](collaboration.md).
- **Many hosts hold records.** A room's records are separate documents, one
  route per app, room and record ([`rooms.md`](rooms.md)), so they can be
  placed independently of governance and of each other. Any number of hosts
  may hold records:
  - **Replicated:** several hosts hold the same records, so the workspace
    stays available while any one of them is off. Adding tilo.coffee as a host
    keeps a workspace reachable around the clock.
  - **Sharded:** each host holds only part of the records, so no single host
    needs the whole workspace.
- **A placement map in the room says which host holds what.** The governor
  writes it. A client reads it and connects to the host that holds the
  record it needs, without asking around. Overhead stays small: one map per
  room, read like any other part of the room document.
- **Adding or removing a host** is a change to the placement map, plus a
  governor handover when the departing host governs. A host leaving never
  takes records with it that no other host holds: the records move first.

### Members hold what they need

A member's device doesn't need every record of a workspace. It holds what
the person opens, plus what is predicted they'll open next, and fetches the
rest from the hosts named in the placement map when it's needed. Records
being separate routes is what makes this cheap.

### One host serves many workspaces

A host, tilo.coffee included, serves many workspaces from one service. No VM,
container image or process per workspace: rooms are data the room service
loads, admits and serves.

Later, tilo.coffee will also run ordinary Linux, Windows and Android apps for
the people in a workspace. Each kind gets its own backend, and each must be:
- efficient: one running app, not a whole desktop or VM per app where that
  can be avoided;
- isolated per workspace: an app in one workspace sees nothing of another;
- reached like any other window in the workspace.

## Devices and identity

- **A person has one identity across their devices and tilo.coffee.**
  Signing in on tilo.coffee, or on a new device, must be quick and pleasant.
  It uses one secret at the sign-in screen, as in
  [`../security/sign-in.md`](../security/sign-in.md), not a key-management
  chore.
- **Devices are added and removed** as part of that identity. A person's
  device key is admitted to the rooms the person is in. Removing a device
  withdraws its key everywhere, and a room it governed passes to one of the
  person's other devices.
- **Device and person are distinct.** Grants name the person; the device
  key says which of their devices acted. That distinction is what lets an
  agent or a host act for a person without becoming them.

## The shared browser

A browser window in a shared workspace is one page, shared, not one page per
viewer and not a video stream of one.

- **The host runs the page.** Its JavaScript and its web state (cookies,
  storage, the live DOM) exist once, on the host, however many devices show
  the window.
- **Devices render it natively.** A device receives the page's HTML and CSS,
  with what the proxy needs to keep scripts' effects in step, and renders
  them itself, as a native frame. It doesn't embed a whole web engine and
  doesn't receive pixels.
- **Local effects stay local.** Hover, focus rings and other presentation
  driven by one person's pointer render on that person's device.
- **Many cursors.** Everyone in the window has their own cursor, as
  elsewhere in a workspace. The page itself sees input as from one user:
  the most recent input wins, for example the last click.
- **One proxy for every web view.** Everyone in a workspace benefits, so
  this lives in this repository beside the browser runtime
  ([`browser-runtime.md`](browser-runtime.md)), not as a separate product.

How the proxy carries script effects to devices is still being designed
with its author.

## Status

This section is a pointer, not a guarantee; the issue tracker has the
details.

- **One workspace hosted on its own device:** the room service on the device
  stewards the workspace, and members join it peer to peer. This is on the
  one-host branch, being brought onto main.
- **The Lounge on tilo.coffee:** a persistent public room on the room
  service, joined by invite. A member sees the history from before they
  joined.
- **Not yet built:**
  - the placement map and record hosting across several hosts;
  - opening any workspace from tilo.coffee after signing in;
  - creating a workspace on another of your devices from tilo.coffee;
  - hosting other workspaces on tilo.coffee;
  - native app backends on tilo.coffee;
  - the shared browser as described above.
