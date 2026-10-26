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

A web page in a workspace is shared like any other window, never streamed as
video when the viewer has an engine of its own. A web member is looking at a
web page and a Tilo device has Chromium, so nearly every viewer does, and
**each viewer renders with the engine it already has**. No web engine is
shipped into a browser, and no engine runs on a server for a viewer that
has one.

**Everything is shared for now.** A link opened in a workspace, posted in
chat or otherwise, opens shared, the way every Tilo app's state is shared.
One workspace is one pile of state, so a page uses the workspace's one
cookie jar; separate sessions per person are added only if they turn out to
be needed.

### Modes

These are the same choices as for Tilo apps (follow someone's viewport, or
see the same state with your own view), plus one that pages need because a
page's state lives in its scripts:

1. **One instance on the host, mirrored.** The page runs once, on the
   workspace's host. Every viewer's engine renders a mirror of its DOM and
   CSS, running none of the page's scripts. This is the default, and the
   only mode a web member's browser gets.
2. **Every viewer runs the page.** Each viewer's engine runs the whole page
   with the workspace's cookie jar. Only the address and playback-like state
   (position, playing, rate) are shared. Only a Tilo device does this,
   because there Tilo controls the engine (see the limits below). It fits a
   static page, a video watched together, and an application with its own
   multiplayer, which then does the collaborating itself.
3. **Follow.** On top of either: one person's scroll, navigation and what
   they have open drive everyone who follows them.

### How mode 1 works

- **The host runs real Chromium, driven over the DevTools protocol.** It
  loads the site directly, from the site's real origin, with the
  workspace's cookies in its own profile, so nothing about the page itself is
  rewritten: logins, service workers, WebSockets and the site's own origin
  checks work as they do in any browser. Input reaches it through the
  protocol's input dispatch, so the page sees trusted events, the same as
  from a real keyboard and mouse.
- **An agent in the page sends the mirror.** It is injected before any page
  script runs. It sends the DOM, the CSS (including constructed and adopted
  stylesheets), shadow roots (it wraps `attachShadow` first, so closed roots
  are seen too) and live form state (values, checked, selection), then their
  changes. Nested frames get an agent of their own; the mirror nests them.
- **A viewer renders the mirror in its own engine.** On the web, that is a
  sandboxed frame on an origin of its own, with scripts disabled. On a Tilo
  device, it is that device's Chromium. Images, fonts and stylesheets come
  from the host's cache through Tilo, not from the site, so the viewer never
  needs the site's cookies. Hover, focus rings, CSS animation, scrolling,
  text selection, fonts and accessibility are therefore native and local.
- **Input goes back attributed.** A viewer's pointer and keyboard events are
  sent to the host with who gave them and dispatched there. The page sees
  input as from one user, the latest input winning. Everyone has their own
  cursor.
- **Typing echoes locally.** A key typed into a field appears at once in
  that viewer's mirror; the host's result replaces it when it arrives.
  - Each person's focus is kept in their own mirror. When they type, the
    host moves the page's focus to their field first, and keeps each
    person's IME composition whole.
- **Video plays in each viewer's engine.** The agent hands over the media
  the page loads, including the segments a site appends through Media Source
  Extensions, still encoded. Each viewer decodes them in its own engine, with
  playback synced. Nothing is re-encoded.
- **Canvas and WebGL are the one thing streamed as pixels.** A DOM mirror
  can't carry what scripts draw, so the host encodes each such element alone
  and the mirror shows it in place.

### What this design can't do, or does only partly

- **DRM video can't be watched together.** A protected stream can't be
  forwarded, since only a licensed engine may decrypt it. It can't be
  captured on the host either: headless Chromium has no Widevine, and
  output protection blocks capture even where it does. A viewer's own engine
  could decrypt only with a licence of its own, which the site issues to
  its own origin and session, not to a mirror. Netflix-type services
  therefore show "can't be shared" in the shared window, and each person
  opens them on their own.
- **Each person scrolls independently only where the page renders the
  content.** One script instance has one viewport. Applications that render
  only the rows in view (virtualized lists: mail, chat, feeds) build rows for
  the host's viewport alone, so a viewer scrolling elsewhere finds nothing
  there. In those, scrolling goes to the host and is shared. Static content
  and ordinary long pages scroll independently.
- **Layout that scripts compute follows the host's viewport.** CSS media
  queries are evaluated in each viewer's mirror, but a script that picks
  mobile or desktop markup from the window size picks it once, for the
  host. The host's viewport follows the person who last interacted.
- **Typing at the same time in different fields is approximate.** Each
  switch moves the page's focus, which fires blur and focus events; a site
  that validates or closes a suggestion list on blur will react to them.
  Typing into the same field at the same time isn't supported.
- **Every interaction run by script costs a round trip to the host.** Hover
  and focus done in CSS are local; a menu opened by a script on
  `mouseenter` waits for the host. On a nearby host this is fast; across
  continents it is noticeable.
- **Mode 2 isn't offered in a web browser.** Running a site's own scripts
  inside a web member's browser needs a rewriting proxy: same-origin rules,
  cookies, service workers, URLs written by scripts, CSP, OAuth and sign-in
  flows. That breaks too many real sites, especially sign-ins, to be worth
  it, so web members always get mode 1.
- **Mode 2 shares one session across devices.** Some sites sign out a
  session used from several devices at once.
- **Each mode 1 page costs the host an engine.** That's a Chromium renderer
  per open shared page, typically 100–300 MB. A person's own host device
  bears its own pages. tilo.coffee bears the Lounge's and, later, those of
  the workspaces it hosts, so it needs a limit per workspace and pages
  nobody is viewing suspended.

### Cases

- **A blog post:** mode 1. Everyone scrolls on their own. Highlights and
  comments are workspace records anchored to the text.
- **A video site:** mode 1. The encoded media is forwarded and each viewer
  plays it in its own engine, in sync. DRM video is the exception above.
- **A stateful application:** mode 1, with the limits above for
  virtualized lists and typing at the same time.
- **An application with its own multiplayer:** mode 1 for now. On Tilo
  devices mode 2 fits better, with everyone appearing as the workspace's
  one account until separate sessions exist.

### What exists

`crates/browser/paint-stream` and `tilo-browser-peer` show a native member's
browser tab to web members as a layered display list that each client draws
with Tilo's renderer, not as video; other legacy windows still reach web
members as video. A display list carries what the page looks like, not its
DOM or CSS, so local hover and focus aren't possible with it. The host
engine, the injected agent and the mirror are not built.

### Open questions

- **Choosing between modes 1 and 2 on Tilo devices.** Shared is the default;
  when mode 2 is chosen, and whether automatically or by the person, is
  undecided.
- **Moving a page to another host.** When a workspace's governing host
  changes, an open page's live state can't move with it. The page reloads
  at the same address on the new host.
- **The display list's future.** Whether `paint-stream` stays as a fallback
  for a viewer without an engine, or goes once the mirror exists.

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
