# Hosted Browser

A workspace hosted on a relay that sells Linux machines (tilo.sh) opens
Browser like any other app. Its pages run in the workspace's own VM, the one
its Linux programs run in, and every member sees them live on the desk.
Agents drive them through Browser's page actions.

## Where it runs

Each hosted workspace has **one VM** on the VM hosts
(`crates/runtime/legacy-vm/src/hosted`), from **one guest image**
(`os/Containerfile.hosted-guest`, pinned as `HOSTED_GUEST_X86_64_REF` in
`os/hosted-guest-snapshot.env`): the workspace's toolchains, and a headless
Tilo session (`--backend headless`, drawing on the CPU: the hosts have no
GPU) with Browser and the Chromium engine runtime it requires.

- **The browser is a process in that VM.** The first browser action wakes
  the VM if it sleeps and starts the session through the guest agent
  (`/usr/libexec/tilo/browser-session`). The session runs as `tilo-browser`,
  an account with no sudo, no shell and none of the tenant's files, with
  Chromium's own sandbox and site isolation on.
- **It ends by itself**: ten minutes without page activity, or two hours
  alive (its device certificate lasts that long). It is asked to end with
  `SIGTERM`, and the VM then sleeps like any idle VM.
- **New VMs are clones.** A host keeps one template per VM size in use: a
  VM booted once from the guest image, its page cache filled with the
  session's, Browser's and Chromium's files, snapshotted and kept read-only.
  A workspace's first wake restores a clone of it: its disk a reflinked copy
  of the template's, its memory file the template's own, mapped privately,
  so the host keeps one copy of every page no clone wrote. A clone's first
  suspend copies that file before writing into it. The template ran no
  session and holds no identity; each clone's session is handed its
  workspace's at the hand-off, and the VM generation ID reseeds each clone's
  randomness when it is restored.
- **Logins persist per workspace.** The profile (cookies, storage) is kept
  among the workspace's own files on the VM host
  (`.tilo/browser/profile.tar.gz`), which sync, encrypted, through the
  workspace like any of its files. The session restores it before it starts
  and writes it back, whole, when it ends. The device's identity, its secrets
  and Chromium's caches are never saved.

The session is **a device of the workspace**, and the narrowest one there
is: the relay certifies a fresh device for it, confines it to that workspace
and that window's record (see [Isolation](#isolation)), and refuses it once
the session stops. It opens the Browser window on the workspace's Browser record,
so the window is live, published and subscribed like a Browser window on
anyone's computer, and its page actions (`browser@<window>/page#read-page`,
…) are the ones Browser already has.

## Isolation

- **One VM per workspace.** No workspace or account shares a VM, a disk, a
  profile, a device or a process with another. A page that gets out of
  Chromium's sandbox lands in an unprivileged account inside a jailed
  microVM ([`job-sandbox.md`](job-sandbox.md#threat-model)).
- **What leaves the VM** is what leaves any device in the workspace: the
  Browser record's changes, action answers, the page's display list for the
  members viewing it, and, for `capture-page`, a picture of the page.
- **Egress is the public internet and the relay.** The hosted VM network
  drops private, shared, link-local (the metadata endpoint), loopback,
  multicast and reserved networks and Azure's platform address. Every VM is
  given exactly one more destination, the relay its session joins through
  (`--relay-endpoint`, from `TILO_VM_RELAY_ENDPOINT`), over UDP and TCP; a
  relay on the host's own loopback (a local stack) is reached at `10.0.2.4`.
  IPv6 is off.
- **Limits** are the VM's: its size's CPU and memory in the jail's cgroup,
  200 Mbit/s each way.
- **The session's device reaches one record and one page.** A page may hold
  the device's key, so the relay confines it to what its window is: the one
  Browser record the open made (`tilo_room_host::environment`), decided where
  the relay certifies the device. In the state plane it is replayed and
  handed only that record's channel and may write no other; on the live
  plane it publishes its own participant namespace (its page track) and is
  offered no one else's track; a remote action it sends is refused unless it
  answers the page action the relay handed it. It still receives the
  workspace's own document (the desk layout, which a session needs to join)
  and the relay's governance and sealed checkpoint frames, none of which it
  can open beyond its record. This is the relay's register beside the agent
  register, not a delegation and not a certificate capability.
- **The residual risk is the key itself:** the workspace's own agent has sudo
  in the VM and can copy the browser device's key. What it gets reaches only
  the Browser record and its page track (no power the agent does not already
  have there) and is refused on every plane once the environment stops, or at
  most two hours later, when the certificate ends; the relay keeps that
  refusal in its own state, so a restart neither widens it nor forgives it.
  The relay sees the stop from the device's own sessions, and cuts their
  connections.

## Placement and routing

The relay places a workspace's Browser where its shells run
([`placed_shells.rs`](../crates/hosting/tilo-relay/src/placed_shells.rs)):

1. Someone opens Browser: a person on the desk, or an agent with a scoped
   key calling `app.so.tilo.browser#open`. The relay asks the placer for the
   host of the workspace's VM; while every host is full the open waits and
   says so once.
2. The relay asks that host's member device, over the device plane, to open
   Browser, handing it the material below. The member asks its hosted-VM
   service, which wakes the VM and starts the session.
3. The session joins the workspace and opens the window; opening Browser
   again attaches to it.

### What the session in the VM is handed

One object (`tilo_relay_protocol::hosted::Material`), written to the
session's standard input as one line:

| Field | What the session does with it |
| :--- | :--- |
| `workspace`, `workspace_id` | The workspace it joins. |
| `record` | The Browser record to open the window on. |
| `device`, `secret` | Its device certificate and signing key: its identity. |
| `relay` | The relay's endpoint, at the one address the VM reaches it by (the VM host rewrites the address, nothing else). |
| `user_public_key` | The person the workspace belongs to. |
| `account` | Who pays, as the relay counts it. |

It reports on its standard output: `ready`, then `up <provider>` once the
window is live (an agent names it `browser@<provider>`), then `used` whenever
the page is used.

Door agents' calls go to the workspace host's device. So the relay forwards
a page action addressed to the hosted Browser window to the device holding
it, as it already carries remote actions between a workspace's devices. The
action runs on the window, and History records it as the caller's: the
agent's run, for the person it acts for.

The window opens for an agent rather than for the person: it browses as a
visitor, on a fresh profile of the VM's own, so an agent reads and fills the
page it opened without anyone being asked, and what acts on the page still
asks. Nobody stands in front of a window in a VM, so the session there
refuses a page action for want of the person's answer, and the relay puts
the question to the person's approving devices itself
(`CertificationProtocol::confirm`, five minutes for this one invocation)
while the caller waits: a caller needs nothing beyond asking and following
what it asked for. Allowed, the relay hands the same invocation to the
window again with the person's answer signed by its own device key; the
session accepts an answer from the host that certified it, for its own
person, and runs the action once. Denied or unanswered, the invocation ends
refused and nothing runs. That is the approval before `submit-page`.

**Every departure is that same approval, however it was triggered.** In a
window opened for an agent the engine stops the page where it was about to
send something and holds it there: a form submission — a submit button, Enter
in a field, the page's own `submit()` or `requestSubmit()` — any navigation or
`fetch`/`XMLHttpRequest` with a method other than `GET` or `HEAD`, from the
page or a frame of it, a download, and what a payment flow sends. The page
stays as it was filled and nothing leaves: the request is paused, not dropped,
so the person who allows it sends exactly what was filled in. The page action
whose run reached the hold answers that it needs the person, naming the method
and where it goes (and, for a download, the file it would save), and their
allow releases that one request — the invocation
does not run the page action again — while a denial or a question nobody
answers drops it and leaves the page as it was. Such a hold is the
invocation's, and the window publishes no second way to answer it, so nobody
is asked about one departure twice.

A departure no action asked for, such as a page posting on its own or a
download the page started, waits on nobody's invocation. The window publishes
it as the wait it is — `send-held-request` and `keep-held-request` of
`browser.page`, each bound to the hold's id and stating it as the argument a
caller names — and the session in the VM reports each one it publishes to the
relay, naming that invocation. The departure travels with the row, so the
question names what leaves and not only a name for it: the page it leaves
from, what it sends and where (`send the form to https://visa.example
(POST)`), and the filled page as a picture the window kept in the workspace's
content store, which the person's device draws before they allow it. The relay
puts it to the person as it puts any page action, and their answer comes back
to the session as the confirmation their allow runs it with: the same witness
the relay signs for a page action, for that one request. Denied or unanswered,
the session runs `keep-held-request` instead and the page stays as it was
filled. A person's own window holds none of this: what they click leaves as
they click it.

A key can carry the authority to operate the pages its workspaces' Browser
holds without asking each time: the person ticks **Operate web pages in
Browser** where they make the key (it is ticked unless they say no, and a key
minted from a key keeps its answer), and the key's certificate carries it.
Holding it, the relay answers for the person on the page actions that
operating a page is — `navigate`, `fill-page`, `select-page`, `click-page`,
`press-page`, `scroll-page`, `drag-page` and `pointer-path`/`pointer-page` —
signing the same witness a person's own answer would, so the window runs one
at once, as that key's agent, and counts it. A hold is untouched by it: the
engine still stops the page where it was about to send something, answers
that invocation that it needs the person, and only their own answer releases
that one request, so a key that clicks a submit button still waits for them.
It answers for nothing else either:
`submit-page`, filling in a file or a password, and everything else Browser
holds for the person still asks them, a key without the scope is asked about
every page action, and revoking the key ends it with the key's certificate.
Where the person sees it: the door's grants page says it on the key, and the
lists of agents let in — the Tilo overview and the Console's Connect page —
say it on the agent's row as it reads the key's certificate at the door, with
the same Revoke.

The page action is counted once the window answers that it ran, so an
action the person did not allow is never counted.

## The paint path

A web page is foreign content, so its drawing is the one thing sent to
viewers: the session publishes the page's paint stream
(`crates/browser/paint-stream`) as one track per Browser window
(`tilo-page/1`) on the workspace's live scope, only while someone views it,
and a member's desk draws it with `ui::webview::paint::PageRenderer`. No
video is streamed.

## Billing

Per action, not per VM:

- **Browser actions** are counted per account and billing period in the
  relay's ledger. The plans file sets the free allowance
  (`sells.browser_free_actions`: 300 a month on tilo.sh). Past it, an account
  without a card is refused with `no_payment_method` and one next step,
  `add_payment_method`. An action the person did not allow is never counted.
- **Linux actions** need a card on a host that sells VMs: a workspace
  without one still gets its VM for Browser, but a Linux command in it is
  refused with `no_payment_method`. The VM's time is billed per second from
  the first Linux action until it sleeps; time it spends only holding the
  Browser is not.

## Cost

Measured with `t test hosted-vm-measure` (the VM host image and this
checkout's guest on a KVM machine; small VMs, a one-form page open in each):

| | Cold (booted) | Clone of the template |
| :--- | ---: | ---: |
| Wake to the agent answering | 4.5 s | 0.12–0.25 s |
| Engine start to `ready` | 5.9 s | 2.4–2.9 s |
| Engine start to the page's first frame | 6.6 s | 2.7–3.3 s |
| Host memory with the page open (PSS) | 615–645 MiB | 320–325 MiB |
| … of which shared with the template | none | about 210 MiB of 530–545 MiB RSS |

A D4s_v5 leaves its VMs 12 GiB: about 38 such clones by what they take.
The host's memory budget counts a VM at its size instead (a small VM is
4 GiB, as its tenant may fill it), so it wakes 3 small VMs there at once, and
a workspace that only holds its Browser counts the same as one compiling.

Per hour, at westus2 prices of November 2026 (`clusters/azure-tilo-sh` in
the infra repo), for a workspace with Browser open:

| Node | Price | Counted at its size (small, 4 GiB) | By what a clone takes (about 325 MiB) |
| :--- | ---: | ---: | ---: |
| Warm D4s_v5 (always up, already paid) | $0.192/h | 3 VMs: $0.064 each | about 38: $0.005 each |
| Overflow D16s_v5, on demand | $0.768/h | 14 VMs: $0.055 each | about 180: $0.004 each |
| E16as_v5 on Spot, once quota is granted | $0.167/h | 31 VMs: $0.005 each | about 380: under $0.001 each |

Browser-only workspaces are mostly idle, so CPU limits a node well before
memory does in the right-hand column. A ten-minute visa form costs about a
cent on demand counted at size, and a tenth of that on Spot.

## Running it

A host that places workspaces (`tilo-relay room … --room-records`) serves
their Browser with `TILO_HOSTED_PLACER`, `TILO_HOSTED_MEMBERS`,
`TILO_HOSTED_ACCOUNT` and `TILO_HOSTED_VM_SIZE` (as for shells), plus
`TILO_HOSTED_BROWSER_RELAY` (`KEY@HOST:PORT`, the public address sessions
reach it at), `TILO_HOSTED_BROWSER_ACTIONS` (the action ledger) and
`TILO_HOSTED_PLANS`. A VM host serves Browser in its VMs when
`TILO_VM_RELAY_ENDPOINT` is set; it keeps its templates under its state
directory by itself.
