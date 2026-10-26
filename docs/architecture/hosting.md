# Hosting: workspaces, hosts, devices and identity

This page is the target for how workspaces are hosted, reached and shared.
Some of it has no interface yet or comes later. The model must still allow
it without a rewrite, so every change to workspaces, hosts, devices or identity is
checked against this page. The workspace itself, holding layout,
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

## Names

This page uses the names the code is moving to:
- **Workspace** for what the code and older docs call a room ("a workspace
  is a room"). "Room" goes away.
- **Resource** for one thing an app keeps in a workspace (a note, a chat
  message, a book), stored side included. "Record" goes away.
- **Part** and **channel** stay: a piece of a large resource, and the path a
  resource's changes travel on.
- **Committed**, **sealed** and **legacy** stop being kinds of workspace or
  code paths (see "Keys" below). **Governor**, **steward** and **native
  owner** stop being roles (see "Governance" below).
- **Relay** and **home relay** stay as they are. The room service is renamed
  for what it is: the host program.

## Hosts

A host is any node that holds some of a workspace's resources: a person's
own device, a home relay, a server, or tilo.coffee. Every host has these
properties. None of them is a type, and a node's role is just their
combination.

### Keys

- **Every resource is end-to-end encrypted, always:** on the wire and at
  rest, on every host.
- **A trusted host holds the workspace's keys,** like any of the workspace's
  devices. It reads, checks changes, searches and can do work.
- **An encrypted host doesn't.** It stores ciphertext and passes it on.
  Behaviour with only an encrypted host reachable is the same as with no
  host at all, except that data can come from it.
- **Where data comes from is one layer below everything else.** A resource
  store fetches channels from whichever hosts hold them. Code above it never
  asks what kind of host that was, only whether some host will take a piece
  of work (below) or it has to do the work itself.
- **Every node with keys checks what it receives,** by the same rules. An
  encrypted host stores what members send, and readers drop what's invalid.

### Always on

A node may say it's always on. That's only a label: it says what the node is
meant for. It doesn't decide where data comes from or where work runs. A
node meant to be occasional can be online all the time, and the other way
round. Decisions use what is actually reachable and how well.

### Taking work

Work is anything expensive done for someone: search, indexing, transcoding,
rendering, an agent's run, running a shared page. Where it runs is decided
each time, from:
- **who will take it:** a person's own trusted devices take their own
  person's work by default; a node set up as a server takes the workspace's
  work;
- **what the node has:** CPU, RAM, GPU, and whether it's plugged in or on
  battery and power saving;
- **whether someone is using it right now,** and how loaded it already is;
- **what the work needs and where that already is:** the resources it reads,
  and what it costs to move them there. A nearby, powerful device that
  already holds a video's files may render it far better than an always-on
  server that would have to fetch them first. Resources likely to be needed
  move before the work is asked for.

Devices don't take work for collaborators by default. They do exchange
resources with them.

### Holding resources

How much of a workspace a node keeps is its own setting, one of:
- **Full mirror:** everything.
- **Sharded:** several nodes agree to keep every resource on enough of them
  that any n of them can fail with nothing lost, a bit like RAID. Which node
  keeps what stays smart: resources stay near where they're used.
- **Cache:** whatever this node needs often or is close to, kept
  opportunistically. On a Tilo device the cache may use a lot of storage
  without asking.

A workspace's placement says which nodes hold what, so a client goes
straight to a node that has what it needs.

### Collaborators' devices

- A device exchanges resources with the devices of a workspace's other
  people, and shows how much it sends to each.
- A person can block exchange with a device that behaves abnormally.
- Data saving applies per connection: on a metered network (mobile data, a
  hotspot) a device fetches only what's needed now.

### Setting up a node

The line between a server and an ordinary device is fluent. Turning any
node into a host for a workspace, for example a small board that takes work
and keeps everything, is a few plain questions during setup: which
workspaces, whether to keep everything or part, whether to take work, and
whether to hold keys. The same questions can be answered again later.

### Search across everything

Search needs plaintext, so the index is built where the keys are. The index
is a resource like any other: encrypted, placed and held like the rest. A
trusted node with capacity builds it, and an always-on server keeps it
current.
- **With a trusted server:** it holds the whole index and answers at once.
- **Without one:** a device fetches only the index parts for the terms
  searched, located by a keyed hash of each term, from wherever they're
  held, and searches locally. An encrypted host sees which parts are
  fetched, but not the terms or the content.

So full-text search over a whole workspace works for everyone. A trusted
server only makes it faster.

### Members hold what they need

A member's device doesn't need every resource of a workspace. It holds what
the person opens, plus what is predicted they'll open next, and fetches the
rest from where placement says it's held.

## Governance

Who is in a workspace, which devices belong to whom, and the workspace's
rules are a signed governance log. Every node keeps it, and every node with
the log checks it with the same deterministic rules. No host is special, and
no node orders governance for the others.

- **Any authorized device can act.** A person enrolls a new device from
  any of their devices.
- **Rules can require several signatures.** For example, removing one of a
  person's devices needs two of their devices once they have three or more.
  Below three, it needs one device and the person's recovery code (below).
  A request without enough signatures waits, and the person's other devices
  ask them to approve it.
- **Keys: a device key per device, an identity key held by the quorum.**
  - The secret typed at sign-in ([`../security/sign-in.md`](../security/sign-in.md))
    unlocks only that device's own key. Someone who learns it gains one
    device's authority, not the person's.
  - Each device key is signed by the person's identity key.
  - The identity key is shared across the person's devices under the same
    quorum rules, so anything signed with it (enrolling a device, removing
    one) takes as many shares as the rules require. Where the rule is one of
    n, which a person can choose, each device holds the key whole.
- **Removal wins.** A removed device's changes are rejected unless they come
  before its removal in history. Undoing what someone changed over a time
  range is a general Tilo feature, used separately. Removal doesn't do it.
- **Keys rotate on removal.** A removed device can read nothing new. What it
  already read can't be taken back.
- **Conflicts resolve the same way everywhere:** removals made concurrently,
  changes to the rules themselves, and quorums that change as devices come
  and go.
- **The recovery code is a share.** The recovery code a person gets at
  setup is one share of their identity key, held outside their devices. It
  counts toward the quorum like a device. While a person has fewer than
  three devices, it is required: removing a device then takes a device and
  the recovery code, so an unlocked stolen device can't act alone. If every
  device is lost, the recovery code and a new device restore the identity.
- **Two devices removing each other freeze the identity.** If two of a
  person's devices each remove the other, neither removal stands and the
  identity is frozen: it signs nothing new until the recovery code decides
  which device stays. Shares decide, not devices, and an identity never
  splits in two.

The scenario this must hold up to: a person has three devices and one is
stolen unlocked. The thief can't remove another of their devices alone,
because the second signature never comes and the person declines on any
device. The person removes the stolen device with two signatures. From
then on, every node rejects its changes.

### One host serves many workspaces

A host, tilo.coffee included, serves many workspaces from one service. No VM,
container image or process per workspace: workspaces are data the host program
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
  device key is admitted to the workspaces the person is in. Removing a
  device withdraws its key everywhere, as described under "Governance".
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

- **The host runs Tilo's own browser engine.** That's the Chromium the
  Browser app already embeds through CEF (`ui-core`'s `webview::chromium`,
  with one host process per profile shared by every process that opens it),
  running off-screen. It is not a second, automation-driven headless
  Chromium: automation leaves fingerprints, such as the webdriver flag and the
  DevTools runtime, that sign-in pages and bot checks refuse, and DevTools
  is a debugging protocol, not an embedding interface. The engine loads the
  site directly, from the site's real origin, with the workspace's cookies
  in its profile, so nothing about the page itself is rewritten: logins,
  service workers, WebSockets and the site's own origin checks work as they
  do in any browser. Input reaches it through CEF's native input path, the
  same one the Browser app uses for a person at the keyboard.
- **One engine integration.** The mirror is a feature of that engine host,
  beside its frames and damage. The Browser app on a Tilo device and a
  shared page on a host are the same code, so a page shared from a device
  and a page run by tilo.coffee behave alike.
- **The host doesn't paint what nobody needs.** Viewers render the mirror
  themselves, so the host keeps rasterising only for canvas and WebGL
  elements and for the page's own needs, at a low frame rate otherwise.
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
- **Text fields are edited collaboratively, then applied to the page.**
  Typing is the one interaction where "the latest input wins" would lose
  work, so fields aren't driven by forwarding keystrokes.
  - **Each field is a shared text.** Each field the page offers (`input`,
    `textarea`, `contenteditable`) is backed, while anyone edits it, by a
    text CRDT keyed by the agent's id for that element. Every viewer edits
    their mirror's field locally, with their own caret and selection, and
    their edits merge into that text. Typing appears at once, and two
    people typing in the same field both keep what they typed, merged
    character by character, with each caret staying where its owner left
    it.
  - **The host applies the merged edits to the real field as typing.** For
    a plain field, it uses the element's own value setter and the input
    events the page expects (`beforeinput`, `input`, with the input type and
    range of each edit), so frameworks that listen for input, React's
    controlled fields among them, see ordinary typing. This needs no focus
    change: two people typing in two different plain fields at once move
    nobody's focus and fire no blur, so validation and suggestion lists
    don't react to the other person.
  - **Rich editors get the edit at its own caret.** A `contenteditable`
    editor (ProseMirror, Slate, Lexical and the like) keeps its own model,
    so the host sets the document selection to the editing person's caret
    in that editor and gives the edit to the engine as native text input,
    which the editor handles as its own typing. Edits from several people in
    the same editor are applied one after another, each at its author's
    caret, mapped through the edits before it.
  - **The page has the last word.** What the page does with an edit (an
    input mask, autocorrect, a length limit, a reformatted number) comes back
    through the mirror, and the agent writes it into the shared text as the
    page's own edit. Each viewer then sees what the page accepted, with
    their caret mapped through it.
  - **Composition stays with its author.** An IME composition shows only in
    its author's mirror until it is committed. Then it is one edit,
    applied whole.
  - **Focus is per person.** Each person's focus is kept in their own
    mirror. The page's focus moves only when someone clicks or tabs to
    something, not because someone else typed.
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
  its own origin and session, not to a mirror. In the shared window, such a
  video's place shows a clear explanation: that this video is protected by
  its service and can't be shown to others, and how to watch it yourself.
  Nothing opens on anyone's behalf.
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
- **Typing at the same time in two different rich editors moves the page's
  focus.** A document has one selection, so applying an edit in one
  `contenteditable` editor and then the other moves focus between them, and
  each move fires blur and focus there. Plain fields aren't affected, nor
  are several people in the same editor. Edits for each editor are batched,
  to keep these moves few.
- **A field the page draws itself isn't a field to the agent.** An editor
  that draws its own text on a canvas and takes keys from a hidden input
  (Google Docs, for one) is edited through that input, as one stream of
  typing. Such applications usually have multiplayer of their own.
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
  per open shared page (Chromium because shared pages are arbitrary
  sites, and compatibility decides), typically 100–300 MB. Suspension (below)
  keeps that bounded.

### Suspending pages

One rule decides when a page stops using memory, wherever it runs: on a
workspace's host, on tilo.coffee, or as a person's own tab on their device.
It is part of the first version, because it is what keeps a host's memory
bounded.

- **Only pages nobody is viewing are suspended, and only when memory is
  needed.** What counts as needed is the one thing that differs between
  hosts:
  - **A shared host** (tilo.coffee, and any host serving several people's
    workspaces) has a budget per workspace. Going over it is a need, so
    pages nobody views are suspended early.
  - **A person's own device** treats only real memory pressure as a need.
    Sixteen tabs on an 8 GB machine stay loaded until memory actually runs
    short.
- **Freeze first.** A frozen page's scripts and timers stop, and its memory
  is kept, so viewing it again resumes it at once, exactly as it was.
- **Unload only if the need persists.** An unloaded page gives back its
  memory. Viewing it again loads it at its address, with its scroll
  position, form contents and the shared text of every field restored. The
  shared texts live in the workspace, so nobody's typing is lost.
- **Never unloaded, only frozen:** a page with something unsaved (one that
  registered a leave-page warning, or with fields someone edited since it
  loaded), a page playing audio or video, and a page holding a call or a
  live connection someone is using. Unloading only ever happens where
  nothing is lost.
- **Least recently viewed goes first.** On a shared host the order is kept
  per workspace, so one workspace's many tabs never unload another's.

### Cases

- **A blog post:** mode 1. Everyone scrolls on their own. Highlights and
  comments are workspace resources anchored to the text.
- **A video site:** mode 1. The encoded media is forwarded and each viewer
  plays it in its own engine, in sync. DRM video is the exception above.
- **A stateful application:** mode 1, with the limit above for
  virtualized lists. Typing at the same time works as described.
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

- **One workspace hosted on its own device:** the host program on the device
  holds the workspace, and members join it peer to peer. This is on the
  one-host branch, being brought onto main.
- **The Lounge on tilo.coffee:** a persistent public workspace on the host
  program, joined by invite. A member sees the history from before they
  joined.
- **Not yet built:**
  - placement and resource holding across several hosts;
  - opening any workspace from tilo.coffee after signing in;
  - creating a workspace on another of your devices from tilo.coffee;
  - hosting other workspaces on tilo.coffee;
  - native app backends on tilo.coffee;
  - the shared browser as described above.
