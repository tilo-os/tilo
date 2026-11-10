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
  requires. It has no compiler, shell tools or language runtimes. CI builds,
  signs and pins it next to the hosted guest
  (`registry.tilo.so/tilo/hosted-browser-guest`). Hosts never build it.
- **Its size** is `browser`: 2 vCPU, 2 GiB, an 8 GiB disk.
- **It keeps nothing.** Its disk is a fresh copy of the image every time it
  starts, it is never snapshotted, and once it has been idle for 10 minutes,
  or alive for 2 hours, it is stopped and its disk deleted. Cookies, storage
  and downloads go with it.
- **It is started directly.** A Linux VM wakes only for a shell run. A
  browser environment is asked for by the relay when someone in the
  workspace opens Browser.

The session in the VM is **a device of the workspace**, like the VM host's
member device: the relay certifies a fresh device for it when it hands the
environment to the workspace, admits that device to that workspace only, and
forgets it when the environment stops. The session opens the Browser window
on the workspace's Browser record, so the window is live, published and
subscribed like a Browser window on anyone's computer, and its page actions
(`browser@<window>/page#read-page`, …) are the ones Browser already has.

Each VM host keeps two browser environments booted with an idle session and
no workspace. Opening Browser takes one of those and hands it to the
workspace, so the window appears in about a second instead of after a boot.
A new spare boots to replace it. An environment serves only the workspace it
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
- **Egress is the public internet only.** This is the hosted VM network
  unchanged: pasta is the only route out, and a bridge filter drops private,
  shared, link-local (including the metadata endpoint), loopback, multicast
  and reserved networks, Azure's platform address, and anything the host
  adds. No cluster-internal address is reachable, so the session reaches the
  relay at its public address, like any member's device. IPv6 is off.
- **Limits:** the jail's cgroup bounds CPU (2 vCPU), memory (2 GiB) and
  processes, Firecracker's rate limiters bound bandwidth (200 Mbit/s), and
  the idle and lifetime limits above bound time.

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

Door agents' calls go to the workspace host's device. So the relay forwards
a page action addressed to the hosted Browser window to the device holding
it, as it already carries remote actions between a workspace's devices. The
action runs on the window, and History records it as the caller's: the
agent's run, for the person it acts for.

Page actions are `Outside`, so an agent's call asks the person first. The
relay already does this, asking the person's approving devices with a
five-minute token bound to the call. That is the approval before
`submit-page`, and it needs nothing new.

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
   gate.
5. **The page track** on the live carrier, and the desk drawing it,
   natively and in a web tab.
6. **The end-to-end test**: a workspace key opens Browser, fills a test form
   added to Browser's test pages (`apps/browser/test-pages`) with a real
   engine, captures it, stops before submitting, a reviewer who joined by
   invite sees the filled page and its History, and the agent submits after
   approval.
