# Printing threat model

Printing is the one desktop capability where the obvious implementation hands
an untrusted application a bidirectional socket to a network daemon running as
another user. It defines what Tilo trusts CUPS with, what it does not, and
which mechanism enforces each part.

The short version: **CUPS runs, confined, reachable only from this machine;
applications never touch it.** An app that wants to print hands the compositor
a file descriptor and asks the user a question. Everything else happens on the
other side of the sandbox boundary.

## Why CUPS is here at all

There is no alternative. Every printer worth supporting speaks IPP or expects
a CUPS filter chain, and reimplementing that is not a project Tilo is going to
finish. So CUPS is a necessary evil: required for the feature, and not
something to be trusted with the run of the machine.

That framing drives everything below. The goal is not "make CUPS safe" — it is
"make a compromised CUPS not matter very much".

## Attack surface

CUPS is a large C codebase that, on a default install, does all of the
following with input it did not author:

- **Parses IPP off a socket.** IPP is a binary protocol with length-prefixed
  attributes. Historically, the parser has been reachable pre-authentication.
- **Reads PPDs and vendor drivers.** A PPD is a program in the sense that
  matters: it names the filters to run and supplies arguments to them.
- **Execs a filter chain over document data.** Ghostscript, image decoders,
  font handling — all fed bytes chosen by whoever submitted the job.
- **`cups-browsed` accepts remote advertisements.** On a default Fedora
  install this daemon listens for DNS-SD announcements and *creates local
  print queues* from them. The queue's device URI and PPD come from the
  network. This is CVE-2024-47076, CVE-2024-47175, CVE-2024-47176 and
  CVE-2024-47177 in one sentence: a machine on your network can cause a print
  queue to exist on yours, and then supply the filter command line that runs
  when someone uses it.

The 2024 chain is the single most useful thing to hold in mind here, because
it was not a memory-safety bug. It was the design working as documented. No
amount of hardening the scheduler would have prevented it; only not running
that daemon does.

## The boundary

### Applications never reach CUPS

This is the load-bearing decision. `/run/cups/cups.sock` is not "the printer".
It is an IPP endpoint that will enumerate destinations, create jobs, cancel
other people's jobs, and — depending on policy — modify printer configuration
including device URIs. Binding it into a sandbox so an app can print a PDF
trades the whole sandbox for one feature.

So no sandbox gets it. Instead:

```
  app  ──fd──▶  Print portal  ──▶  user prompt  ──▶  spooler  ──▶  cupsd
(untrusted)     (compositor)      (OS chrome)      (compositor)   (confined)
```

The app calls `org.freedesktop.impl.portal.Print` (`crates/session/shell/src/portal/print.rs`)
and gets exactly two abilities: hand over **bytes** via a file descriptor, and
ask **the user** to send them somewhere. It cannot name a destination the
user's CUPS does not already have, cannot speak IPP, and cannot see the
printer list without consent.

Enforcement, concretely:

- `Capability::Print` never becomes a bind: `tilo-launcher` turns grants into
  binds in `launcher::capabilities`, which binds only filesystem paths,
  devices and network access. Granting print permission — at any scope,
  however broadly — adds no bind mount. There is no code path that puts the
  cups socket inside a sandbox, which is why `os/tests/printing_isolation.sh`
  greps the launcher sources for one and fails if it appears.
- Print is its own capability variant rather than a reused filesystem or
  device grant, so a permission the user gave for something else cannot
  silently authorise printing.
- The document is read from the fd by the compositor, capped, and spooled by
  argv — never a shell string. The job title is app-supplied and untrusted, so
  it is sanitised before it reaches a command line.

### The scheduler is confined

`os/usr/lib/systemd/system/cups.service.d/tilo-confinement.conf`. Fedora ships
cups.service with no hardening at all; `systemd-analyze security` scores it
**9.6 UNSAFE**. With the drop-in it scores **2.8 OK**, measured offline against
the same unit file.

The relevant parts and their purpose are:

| Mechanism | What it denies a compromised cupsd |
|---|---|
| `ProtectSystem=strict` + narrow `ReadWritePaths` | Writing anywhere except the spool, cache, log, `/run/cups` and `/etc/cups` |
| `ProtectHome=yes` | Reading any user's files. Jobs arrive as bytes over the socket, so this costs nothing |
| `CapabilityBoundingSet` (8 caps kept) | `CAP_SYS_ADMIN`, `CAP_NET_ADMIN`, module loading, binding privileged ports |
| `DevicePolicy=closed` + explicit `DeviceAllow` | Every device except USB and parallel printer nodes |
| `RestrictNamespaces=yes` | Creating namespaces — a sandbox-escape primitive, not a printing feature |
| `SystemCallFilter` | mount, swap, reboot, raw I/O, module loading, kernel debug |
| `MemoryDenyWriteExecute=yes` | Mapping writable-executable memory in the filter chain |
| `NoNewPrivileges=yes` | Regaining privilege through a setuid binary |
| `PrivateTmp` + `UMask=0077` | Leaving readable spool leftovers for other local users |
| `TasksMax` / `MemoryMax` | Taking the machine down with a runaway filter chain |

### Two deliberate non-hardenings

Both would look better on a scorecard and both would break printing.

**cupsd runs as root.** Upstream cupsd must start as root: it creates the
socket with the right ownership and `setuid()`s per job so filters run as the
*submitting* user rather than pooling every user's documents under one
account. `User=lp` on the unit makes it fail at startup, which surfaces as
"printing is broken" long after whoever set it has moved on. The dedicated
unprivileged account is applied where it matters — `User lp` / `Group
lp` in `cupsd.conf`, governing the filter processes that touch document bytes
and vendor PPD code. Root is contained by the table above instead.

**`PrivateDevices` is off.** It replaces `/dev` with a pseudo-only tree, and
USB printers live at `/dev/usb/lp*`. Setting it silently breaks every directly
attached printer. `DevicePolicy=closed` with an explicit allow list achieves
the containment without that cost.

Similarly, `@privileged` is not denied in the syscall filter, because cupsd's
per-job `setuid()`/`setgid()` lives in that set. The specific families it has
no use for are denied individually.

### Nothing listens on the network

`cups.socket` is pinned to `/run/cups/cups.sock` and `cupsd.conf` has no
`Port` and no address `Listen`. The socket is enabled; the service is not, so
cupsd is socket-activated and stays stopped on a machine that never prints —
the IPP parser is not running for most of the fleet's uptime.

The firewall opens nothing for printing. `ipp` and `ipp-client` are absent from
the zone configuration deliberately, with a comment in `os/Containerfile`
saying so, because "we forgot to add it" and "we decided not to" look
identical six months later.

## The discovery decision

**mDNS/DNS-SD printer discovery is OFF, and this was a decision, not an
oversight.**

- `cups-browsed` is **not installed**. It is the component that accepts remote
  advertisements and materialises local queues from them, and it is where the
  2024 chain lived.
- `avahi` is **not installed**, so DNS-SD has no transport on this image even
  if something asked for it.
- `Browsing Off` and `DefaultShared No` in `cupsd.conf` turn off the
  advertising half, so the machine does not announce its own printers either.

**What the user gives up:** network printers do not appear on their own. Adding
one is a deliberate act — you supply its address.

**What they get:** nothing on the network can cause a print queue to exist on
this machine. That is the property the CVEs took away, and it is worth more
than the convenience.

**If someone wants it back:** installing `cups-browsed` and `avahi` and
removing `Browsing Off` restores upstream behaviour. That should be an
explicit, argued change with this section updated, not a quiet package
addition. `os/tests/printing_isolation.sh` fails if any of it reappears.

## What a compromised cupsd reaches

Assume an attacker gets code execution as cupsd — through the IPP parser, a
malicious PPD, or a filter bug. They have:

- Root, but with a bounding set of eight capabilities and `NoNewPrivileges`.
- Write access to `/etc/cups`, the spool, the cache, the log and `/run/cups`,
  and nothing else. `/usr` is read-only under bootc regardless.
- The documents currently in the spool. `PreserveJobFiles No` means finished
  jobs are not kept, so this is jobs in flight, not a history.
- Outbound network. Reaching a network printer is the job, so this cannot be
  removed without removing the feature.
- No user home directories. No other users' processes beyond signalling. No
  module loading, no namespaces, no mounts, no raw I/O.

What they do **not** get: any application's sandbox, the compositor, the user's
files, or persistence in `/usr`. On a bootc image an attacker who cannot write
`/usr` and cannot load a module has no durable foothold outside `/var` and
`/etc/cups`.

## Residual risk, knowingly accepted

1. **cupsd is root.** Confined, but root. The alternative breaks printing; see
   above. If upstream ever supports a rootless cupsd, this is the
   first thing to revisit.
2. **`/etc/cups` is writable by the scheduler.** It must be, or adding a
   printer fails. An attacker who owns cupsd can therefore persist a malicious
   printer definition across reboots. Detecting that is not currently done.
3. **Outbound network is unrestricted.** `IPAddressDeny=` is empty because a
   printer can be at any address on the LAN. A compromised cupsd can talk to
   anything reachable. A per-printer address allow list would fix this and does
   not exist.
4. **`MemoryDenyWriteExecute` is not proven against every filter.** It is set,
   and it is the directive most likely to break an exotic filter. It has not
   been tested against a full filter chain on real hardware. If a printer stops
   working after a filter update, test with that line removed before blaming
   anything else — and record the result here.
5. **The filter chain itself is unsandboxed beyond the service's own
   confinement.** Filters inherit the unit's restrictions but are not
   individually isolated from each other or from the spool.

## Out of scope

- **Printer sharing.** Tilo does not serve printers to other machines. That
  would mean exposing an IPP parser to the network, which is the surface this
  document exists to avoid.
- **Scanning.** SANE is a separate stack with its own problems; none of this
  applies to it.
- **Print accounting, quotas, and per-user policy** beyond `MaxJobsPerUser`.
- **Confidentiality of the printed artifact.** Once bytes reach a printer,
  what happens to them is outside any boundary Tilo controls.

## Where this is enforced

| Property | Enforced by |
|---|---|
| Apps never get the cups socket | No print binding in `launcher::capabilities`; asserted by `os/tests/printing_isolation.sh` |
| Apps print via a consented portal | `crates/session/shell/src/portal/print.rs`, through `flow::resolve` |
| Scheduler confinement | `os/usr/lib/systemd/system/cups.service.d/tilo-confinement.conf` |
| No network listener | `os/usr/lib/systemd/system/cups.socket.d/tilo-local-only.conf`, `os/etc/cups/cupsd.conf` |
| Discovery off | `cups-browsed` and `avahi` not installed; `Browsing Off` |
| Filters unprivileged | `User lp` / `Group lp` in `os/etc/cups/cupsd.conf` |
| All of the above stay true | `os/tests/printing_isolation.sh`, which fails rather than skips when it cannot observe its subject |
