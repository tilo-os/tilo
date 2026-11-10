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
Linux VM. A browser environment is that service's VM with three
differences:

- **Its image** is the browser guest: the hosted guest's kernel and init,
  the Chromium engine runtime Browser requires, and Browser's engine host
  (`apps/browser/chromium`). CI builds, signs and pins it next to the
  hosted guest (`registry.tilo.so/tilo/hosted-browser-guest`). Hosts never
  build it.
- **Its size** is `browser`: 2 vCPU, 2 GiB, an 8 GiB disk.
- **It keeps nothing.** Its disk is a fresh copy of the image every time it
  starts, it is never snapshotted, and once it has been idle for 10 minutes,
  or alive for 2 hours, it is stopped and its disk deleted. Cookies, storage
  and downloads go with it.

Each VM host keeps two browser environments booted and unassigned, with
Chromium started on an empty profile. Opening Browser takes one of those
and hands it to the workspace, so the first page loads in about a second
instead of after a boot. A new spare boots to replace it. An environment
serves only the workspace it was handed to, and it is never handed back to
the pool.

### Why a microVM, not a container

Web pages are strangers' code, and Chromium runs a JIT on them. The job
sandbox's threat model ([`job-sandbox.md`](job-sandbox.md#threat-model)) sends
every host that runs strangers' code to a VM per workspace, so that getting
out takes a hypervisor escape, not a kernel bug. The VM hosts already give
each workspace a VM with everything this needs: a jail per VM, a public-only
network, bandwidth limits, eviction handling, a memory budget, and a placer.
A container pool would be a second mechanism next to it with weaker
isolation. A booted spare answers as fast as a container would start.

## Isolation

- **No shared profile.** One environment per workspace session, and every
  environment starts from the image. No workspace or account ever shares a
  profile, a disk or a process with another.
- **Cookies and storage stay in the VM.** The profile lives on the VM's disk.
  What leaves the VM is the engine's protocol: page events, layout
  snapshots, action answers and, for `capture-page` only, a picture of the
  page. The relay never receives a cookie, a stored value or a request
  header.
- **Egress is the public internet only.** This is the hosted VM network
  unchanged: pasta is the only route out, and a bridge filter drops private,
  shared, link-local (including the metadata endpoint), loopback, multicast
  and reserved networks, Azure's platform address, and anything the host
  adds. So no cluster-internal address is reachable. IPv6 is off.
- **Limits:** the jail's cgroup bounds CPU (2 vCPU), memory (2 GiB) and
  processes, Firecracker's rate limiters bound bandwidth, and the idle and
  lifetime limits above bound time.

## Placement

The relay places a workspace's Browser the way it places a shell
([`crates/hosting/tilo-relay/src/placed_shells.rs`](../crates/hosting/tilo-relay/src/placed_shells.rs)):

1. It asks the placer for the host of `<workspace>/browser`
   (`Request::Place`, environment `browser`, size `browser`). The placer
   counts browser environments in each host's memory budget like any VM,
   prefers a host with a spare, and starts another host only when the budget
   says so. While every host is full, the open waits and says so once.
2. It opens a stream to that host's member device, on the iroh connection
   the device plane already holds to it, as this relay's device. The
   member asks its hosted-VM service to start the engine host in the
   environment with its standard input and output relayed over vsock
   (`legacy_agent_protocol::Launch`, `Stdio::Relay`).
3. Browser's engine on the relay (`ui::webview::chromium::ChromiumEngine`)
   speaks its host protocol over that stream instead of a child process's
   pipes. Nothing else in the app changes.

Browser's record, its window, its page actions, their admission and History
stay on the relay, held by the app worker like every other hosted app
(`tilo_agent_participant::apps`). Only the engine host is elsewhere. This is
the same split as Terminal, where the session record lives on the relay and
only the command runs on the VM host.

## The paint path

The engine host runs every tab hidden, so it paints no frames. The relay's
engine wraps it in `ui::webview::paint::PaintEngine`, which reads the
page's layout whenever it may have changed and turns it into a paint-stream
frame and patches (`crates/browser/paint-stream`). Images travel by content
hash, read from the engine's cache.

The relay publishes those payloads on the workspace's live scope as one
track per Browser record (`tilo-page/1`), on the same MoQ carrier that calls
and window shares use (`crates/collab/tilo-live`). A member's desk shows the
Browser window by subscribing to the track and drawing it with
`ui::webview::paint::PageRenderer`, the same Vello and text code natively and
in a web tab. A viewer that joins late gets a keyframe. No video is
streamed, and a page that nobody watches publishes nothing.

The agent drives the page through Browser's page actions (`read-page`,
`fill-page`, `capture-page`, `submit-page`, …) over the workspace's scoped
MCP and SDK, as on any device. Each one is an invocation on the Browser
record, so History shows it as the agent's run. `submit-page` reaches
outside the workspace, so the person is asked before it runs.

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

See the commit history touching this file and the files it links. Each part
above is a unit:

1. **Metering and the gate** on the relay, with the plans file's allowance.
2. **The engine's host link**: the Chromium engine driving a host over a
   byte stream as well as a child process.
3. **Browser environments** in the hosted-VM service: the `browser` size,
   no persistence, idle and lifetime limits, spares, and the member's
   stream to the engine host.
4. **The browser guest image** in CI.
5. **The relay's Browser**: the app in the hosted catalogue, its engine
   placed through 1–3.
6. **The page track** on the live carrier, and the desk drawing it.
7. **The end-to-end test**: a workspace key opens Browser, fills a test form
   added to Browser's test pages (`apps/browser/test-pages`), captures it, stops before
   submitting, a reviewer who joined by invite sees the filled page and its
   History, and the agent submits after approval.
