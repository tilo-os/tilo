# Hosted Browser

A workspace hosted on a relay that sells Linux machines (tilo.sh) opens
Browser like any other app. Its pages run in a short-lived VM of the
workspace's own, beside its Linux VM, and every member sees them live on the
desk. Agents drive them through Browser's page actions. This note covers
where the engine runs, how it is isolated, how the relay places it, how
members see the page, how actions are counted, and what it costs.

A hosted workspace has three pools:

| Pool | Runs | Pays |
| :--- | :--- | :--- |
| The relay's room host | Tilo apps' actions, in our own code | free, no card |
| Linux VMs on the VM hosts | Terminal, shells, Linux programs | needs a card |
| Browser VMs on the VM hosts | web pages | free up to N actions, then needs a card |

## Where it runs

Each workspace that opens Browser gets one **browser environment**: a
Firecracker microVM named `<workspace>/browser` on the VM hosts
(`crates/runtime/legacy-vm/src/hosted`), which already run each workspace's
Linux VM. A browser environment is that service's VM with these differences:

- **Its image** is the browser guest: the hosted guest's kernel and init, a
  headless Tilo session (`--backend headless`, rendering on the CPU: the
  hosts have no GPU), Browser, and the Chromium engine runtime Browser
  requires. It has no compiler, shell tools or language runtimes. What it runs
  is the Tilo OS's own userspace, so its root filesystem is the Fedora base the
  OS image is built from (`os/base-snapshot.env`) rather than the workspace
  guest's Debian: the session, the runtime's engines and Browser are compiled
  against that base's libc, and no other distribution's loader can resolve them.
  CI builds, signs and pins it next to the hosted guest
  (`registry.tilo.so/tilo/hosted-browser-guest`, pinned as
  `HOSTED_BROWSER_GUEST_X86_64_REF` in `os/hosted-guest-snapshot.env`). Hosts
  never build it.
- **Its session** starts from `/usr/libexec/tilo/browser-session`, the
  image's own entry, which the guest agent runs: it makes the runtime
  directory a login would have, points the session at the image's software
  Vulkan driver (the hosts have no GPU), and starts `tilo-session --backend
  headless --no-x11` on a bus of its own. The workspace it joined, and the
  window it opened, reach the session as one line of material on its standard
  input, and it reports on its standard output: `ready` once its engine is up
  and it waits for a workspace, `up <provider>` once it has joined and the
  window is live, and `used` whenever the page is used. What a person
  operating a host sets is under ["Running it"](#running-it).
- **Its size** is `browser`: 2 vCPU, 2 GiB, an 8 GiB disk.
- **It keeps nothing.** Its disk is a fresh copy of the image every time it
  starts, it is never snapshotted, and once it has been idle for 10 minutes,
  or alive for 2 hours, it is stopped and its disk deleted. Cookies, storage
  and downloads go with it.
- **It is started directly.** A Linux VM wakes only for a shell run. A
  browser environment is asked for by the relay when someone in the
  workspace opens Browser.

The session in the VM is **a device of the workspace**, like the VM host's
member device, and the narrowest one there is: the relay certifies a fresh
device for it when it hands the environment to the workspace, confines it to
that workspace and that window's record (see [Isolation](#isolation)), and
forgets it when the environment stops. The session opens the Browser window
on the workspace's Browser record, so the window is live, published and
subscribed like a Browser window on anyone's computer, and its page actions
(`browser@<window>/page#read-page`, …) are the ones Browser already has.

Each VM host keeps two browser environments booted and no workspace on them.
A session reads its identity once, as it starts, so a spare's session has not
started yet: the host boots the VM, its guest agent answers, and the session
starts at the hand-off, with the material as the first line of its input.
Opening Browser takes a spare and hands it to the workspace, so the window
appears about as fast as a booted session starts instead of after a boot. A
new spare boots to replace it. An environment serves only the workspace it
was handed to, and it is never handed back to the pool.

### Why a session in a VM

- **The relay cannot hold the window.** It runs no apps. Its record workers
  are confined processes with no sockets, no exec and no files, and Browser's
  page actions exist only on a live Browser window. Opening Browser on the
  relay today would make a record and nothing else.
- **A session in the VM is what any device already is.** Browser runs
  unchanged, against a real engine, and the workspace sees another device
  with a window. There is no remote engine protocol, and pixels never leave
  the VM.
- **A microVM, not a container.** Web pages are strangers' code, and
  Chromium runs a JIT on them. The job sandbox's threat model
  ([`job-sandbox.md`](job-sandbox.md#threat-model)) sends every host that
  runs strangers' code to a VM per workspace, so getting out takes a
  hypervisor escape, not a kernel bug. The VM hosts already give each
  workspace a VM with a jail, a public-only network, bandwidth limits,
  eviction handling, a memory budget and a placer. A container pool would be
  a second mechanism with weaker isolation, and a booted spare answers as
  fast as a container would start.

## Isolation

- **No shared profile.** One environment per workspace session, every one
  started from the image. No workspace or account ever shares a profile, a
  disk, a device or a process with another.
- **Cookies and storage stay in the VM.** The profile lives on the VM's
  disk. What leaves the VM is what leaves any device in the workspace: the
  Browser record's changes, action answers, the page's display list for the
  members viewing it, and, for `capture-page`, a picture of the page. No
  cookie, stored value or request header leaves.
- **Egress is the public internet and the relay.** This is the hosted VM
  network unchanged: pasta is the only route out, and a bridge filter drops
  private, shared, link-local (including the metadata endpoint), loopback,
  multicast and reserved networks, Azure's platform address, and anything the
  host adds. A browser environment's session has to join its workspace
  through the relay, which on a hosted stack is a cluster-internal address,
  so that VM is given exactly one more destination: the VM host is told the
  relay's endpoint (`--relay-endpoint`, from `TILO_VM_RELAY_ENDPOINT`) and
  allows that address and port, over UDP (the device plane's QUIC) and TCP.
  Every other cluster-internal address stays dropped, and a Linux VM is given
  none. A relay the VM can dial as it is (a public address, or the cluster
  address of a stack that hosts it) is dialled as it is; a relay on the
  host's own loopback, on a local stack, is reached at `10.0.2.4`, which
  pasta maps to that loopback. IPv6 is off.
- **Limits:** the jail's cgroup bounds CPU (2 vCPU), memory (2 GiB) and
  processes, Firecracker's rate limiters bound bandwidth (200 Mbit/s), and
  the idle and lifetime limits above bound time.

### The environment's device reaches one record and one page

A web page is strangers' code and Chromium runs a JIT on it, so the session
in the VM has to be treated as a device a page may already hold the key of.
The relay therefore confines it to what its window is: one workspace and the
one Browser record the open made (`tilo_room_host::environment`). The
confinement is the relay's, decided where the relay certifies the device and
asking the session in the VM nothing; a page that escapes Chromium and takes
the device's key finds the same narrow door as the session does.

Every plane of the workspace asks it. In the relay's state plane — where a
device syncs a workspace's records — a confined device is replayed the
history of its own record's channel and no other, is handed none of another
record's live changes, is refused when it opens another record, and is
refused when it writes to another channel. On the live plane it publishes its
own participant namespace, where its window's page track lives, and is
offered no track of anyone else's — no call, no camera, no screen, no other
window — while a viewer's ask for its page arrives as a demand on that track
rather than as a subscription of its own. A remote action it sends is refused
unless it answers the page action the relay handed it, so a window's device
calls no door of the workspace; compute, environments, content discovery and
the workspace's administration are refused outright. What is left is what
[Built and what is not](#what-is-built-and-what-is-not)'s fourth unit needs:
join, sync its record, publish its page, answer.

This is the register the relay keeps for the devices it confines, beside the
agent register that confines an agent to its delegation. It is not a
delegation — those are signed by the person's root, which a host does not
hold — and not a certificate capability: the record is a fact of one
placement, made by the relay when someone opens Browser, and the next open in
that workspace may make another.

Two things reach it besides its own record, and both are deliberate. It
receives the workspace's own document — the desk layout, which names the
records it may not read — because a session cannot join a workspace without
it; and it receives the frames the relay itself commits to the workspace,
which are governance and sealed compaction checkpoints. Neither carries
content it could open: record content is sealed under each record channel's
keys, and the device holds no other record's.

When the environment stops, its device is forgotten from the workspace and
from that register, and the session that held it has nothing left to reach.

## Placement and routing

The relay places a workspace's Browser the way it places a shell
([`crates/hosting/tilo-relay/src/placed_shells.rs`](../crates/hosting/tilo-relay/src/placed_shells.rs)):

1. Someone in the workspace opens Browser: a person on the desk, or an agent
   with a scoped key calling `app.so.tilo.browser#open`. In a hosted
   workspace the relay answers that by asking the placer for the host of
   `<workspace>/browser` (size `browser`). The placer counts browser
   environments in each host's memory budget like any VM and starts another
   host only when the budget says so. While every host is full, the open
   waits and says so once.
2. The relay asks that host's member device, over the device plane, to start
   the environment for the workspace, handing it the new device's
   certificate and the record to open. The member hands both to the session
   in the VM through the guest agent.
3. The session joins the workspace and opens the Browser window on the
   record. The window counts as the workspace's Browser for as long as the
   environment runs. Opening Browser again attaches to it.

### What the session in the VM is handed

The relay certifies the environment's device — a member device of the
workspace's person, made with the stand-in the person gave the host, lasting
as long as the environment may run — lists it in that one workspace, and asks
the VM host to start the environment. What the session in the VM is started
with is the relay's placement record, one object
(`tilo_relay_protocol::hosted::Material`), written to its standard input as
one line:

| Field | What the session does with it |
| :--- | :--- |
| `workspace` | The workspace's UUID: the one it joins. |
| `workspace_id` | The same workspace as the relay carries it. |
| `record` | The Browser record to open the window on. |
| `device`, `secret` | Its device certificate and the signing key: the session's identity. |
| `relay` | The relay's endpoint: its id, at the one address the VM reaches it by. The VM host replaces the address the relay named with the one that VM dials (the relay's own, or `10.0.2.4` for a relay on the host's loopback), and changes nothing else of the material. |
| `user_public_key` | The person the workspace belongs to. |
| `account` | Who pays, as the relay counts it. |

The session starts headless (`--backend headless`), with that device as its
identity, joins the workspace through `relay`, and opens Browser on `record`,
then reports the window's provider (`up 42`) so the relay can route page
actions and the open can answer. What it may then use of the workspace is
exactly the scope above: the record `record` names (its channel is
`neighbour_wire::app_document_channel("so.tilo.browser", record)`), the keys
of that record's channel, its own participant namespace on the workspace's
live plane — where it publishes `page/<tab>`, one track for its window — and
the incoming page actions it answers. It subscribes to no one: a viewer asks
for its page by demanding its track, which the relay serves from the
publisher's side, so the session hears the ask without watching the
workspace. An agent names that window `browser@42`; the
provider token itself is the number, which is what every live window of the
desk publishes under. It keeps the window live and published until it stops.

Door agents' calls go to the workspace host's device. So the relay forwards
a page action addressed to the hosted Browser window to the device holding
it, as it already carries remote actions between a workspace's devices. The
action runs on the window, and History records it as the caller's: the
agent's run, for the person it acts for.

Page actions are `Outside`, so an agent's call asks the person first. Nobody
stands in front of a window in a VM, so the session there refuses such an
action for want of the person's answer, and the relay puts the question to
the person's approving devices (`CertificationProtocol::confirm`, five
minutes for this one invocation). Allowed, the relay hands the action back
with the person's answer signed by its own device key; the session accepts
an answer from the host that certified it, for its own person, and runs the
action without asking again. Denied or unanswered, the action does not run.
That is the approval before `submit-page`.

The page action is counted once the window answers that it ran, so an
action the person did not allow is never counted.

## The paint path

A web page is foreign content, like a legacy X11 window, so it is the one
kind of window whose drawing is sent to viewers. Tilo apps' own UI is never
streamed. The Browser window already offers its page as a paint stream
(`ui::webview::paint::PaintEngine::streaming`, `PaintStream` on the floating
surface): a frame of paint-ordered items, then patches, with images sent once
by content hash (`crates/browser/paint-stream`). The session in the VM
publishes it on the workspace's live scope as one track per Browser window
(`tilo-page/1`), on the same MoQ carrier calls and window shares use
(`crates/collab/tilo-live`), only while someone views it.

A member's desk shows the hosted Browser window by subscribing to that
track and drawing it with `ui::webview::paint::PageRenderer`, the same Vello
and text code natively and in a web tab. A viewer who joins late gets a
keyframe first. No video is streamed.

## Metering

The relay counts every page action it runs on a hosted Browser, per account
and billing period, in a ledger beside the workspace state. The plans file
sets the free allowance (`sells.browser_free_actions`). An account the plans
file names has a card on file and is never stopped. Any other account may
run that many actions a period. After that, each further action is refused
with the VM path's refusal: reason `no_payment_method`, a sentence saying
how many free actions the period had and that a card continues, and one next
step, `add_payment_method`, linking to the provider's page. Once the provider
adds the account to the plans file, the next action runs. A host whose plans
file sells nothing counts nothing and refuses nothing. The account's live
standing (`tilo.hosting`, `host.account/account`) shows the count and the
allowance.

Prices, checkout and the card page are the provider's, not this code's.

## Cost

Prices are for westus2, November 2026, from `clusters/azure-tilo-sh` in the
infra repo.

| Where | Node | Browser environments it fits | Per browser-hour |
| :--- | :--- | :--- | :--- |
| Warm host (always up, already paid) | D4s_v5, $0.192/h, 12 GiB budget | 2 spares + what Linux VMs leave | $0 extra |
| Overflow, on demand (today) | D16s_v5, $0.768/h, ~58 GiB budget | ~25 | ~$0.03 |
| Overflow, Spot (once quota is granted) | E16as_v5, $0.167/h, ~124 GiB budget | ~50 | ~$0.003–0.004 |

The counts are by memory (2 GiB plus the jail's overhead), with CPU shared
4:1: a browser filling a form is mostly idle. A ten-minute session like a
visa form costs about half a cent on demand and a twentieth of a cent on
Spot. Nothing new is provisioned. Browser environments use the existing
`tilosh` pool, which scales to zero, and the always-up warm node. The two
spares take 4 GiB of the warm node's 12 GiB budget.

## Running it

A host that places workspaces (`tilo-relay room … --room-records`) serves
their Browser when these are set, all of them read from its environment:

| Variable | What it names |
| :--- | :--- |
| `TILO_HOSTED_PLACER` | The placer's address (`vm-placer:7702`): where a workspace's environments are placed. |
| `TILO_HOSTED_MEMBERS` | The VM hosts' member devices: a JSON map of host name to its device key and address. |
| `TILO_HOSTED_ACCOUNT` | The account the machines run for. |
| `TILO_HOSTED_VM_SIZE` | The size a workspace's Linux VM runs at; browser environments are always `browser`. |
| `TILO_HOSTED_BROWSER_RELAY` | `KEY@HOST:PORT`: this host's device key and the **public** address the sessions in its environments reach it at. Their network reaches the public internet and no private, cluster-internal or link-local address, so this is never the host's own. Required: the host refuses to start without it. |
| `TILO_HOSTED_BROWSER_ACTIONS` | The ledger file the page actions are counted in. |
| `TILO_HOSTED_PLANS` | The plans file: what a period's free page actions are (`sells.browser_free_actions`) and which accounts have a card. |

A host that runs the environments — a VM host — adds these to the member
device's own environment:

| Variable | What it names |
| :--- | :--- |
| `TILO_HOSTED_VM_SOCKET` | The VM host's own hosted-VM service, which the member asks for an environment over its socket. |
| `TILO_HOSTED_PLANS` | The plans file the service admits by. |

and starts that service with `--browser`, the browser guest image
(`--browser-kernel REF --browser-disk-image PATH`), and optionally
`--browser-spares N`, `--browser-idle-stop SECONDS`, `--browser-lifetime
SECONDS`. The image is the digest CI pins as `HOSTED_BROWSER_GUEST_X86_64_REF`
in `os/hosted-guest-snapshot.env`; a host that runs no image runs no
environments and says so when someone opens Browser.

## What is built and what is not

Each part above is a unit:

1. **Metering and the gate** on the relay, with the plans file's allowance.
2. **Browser environments** in the hosted-VM service: the `browser` size, a
   direct start, no persistence, idle and lifetime limits, spares.
3. **The browser guest image** in CI: a headless session, Browser and the
   engine runtime.
4. **Joining and routing**: `#open` in a hosted workspace places an
   environment, its session joins as a device of that workspace only and
   opens the window, and the relay routes page actions to it through the
   gate. The relay's half is in and runs where its environment names it
   (`tilo_relay::hosted_browser`); the session in the environment does not
   read the material yet, so no environment joins a workspace.
5. **The page track** on the live carrier, and the desk drawing it,
   natively and in a web tab.
6. **The end-to-end test**: a workspace key opens Browser, fills a test form
   added to Browser's test pages (`apps/browser/test-pages`) with a real
   engine, captures it, stops before submitting, a reviewer who joined by
   invite sees the filled page and its History, and the agent submits after
   approval.
