# Testing tilo.sh on your machine

tilo.sh is tested on a developer's machine (or a remote build machine) before any
release. It is never tested on the deployed site. One command starts the
whole product from your checkout. It uses the same entrypoints, Caddyfiles
and environment variables as production:

```sh
os/containers/tilo-sh/stack.sh up
```

By default the stack runs this checkout's programs as plain processes.
`t build` compiles them on the build server's warm, incremental build and
puts them in `./target`, so only what a change touches compiles. Each
service gets its own state and runtime directory under the stack's state.
The first run also builds the web bundle and fetches the font catalog; after
that, `up` takes as long as the programs take to build. `stack.sh help`
lists every command.

`stack.sh up --images` runs the release images instead: `image-lounge`,
`image-front-door` and `image-web-entry`, built by `t` for this commit,
under rootless podman. That is the rehearsal of what ships: the images as
they are packaged, which their own checks (`image-lounge`, `image-web-entry`,
`test-image-front-door`) hold to booting, answering their health route and
one request per route, and which the release publishes. `t test e2e` does not
wait for them: it runs this commit's programs.

## Checking a change end to end

`t test e2e` is the whole product, used as people use it, on the programs this
commit builds: the check compiles nothing and starts no container. Everything
it runs is another check's output — `tilo-sh-binaries` (the relay's context
and the door), `tilo-sh-caddy`, `image-web-entry-context` (the site, laid out
as the web entry image lays it out), `lounge-gate-*` (the relay, the command
and the tools the page's own flows run) and the web bundle — handed to
`stack.sh` as `TILO_SH_LOUNGE`, `TILO_SH_PROGRAMS`, `TILO_SH_SITE` and
`TILO_SH_CADDY`. The stack is brought up once, and then every flow runs at
the same time against its site, relay and door: the page's own flows
(`os/containers/tilo-lounge/test-e2e.sh`) and this check's native steps, as
flows of that runner (`check.sh --flow signin|visa|entry|passkey`). A change
to a step reruns the run, and a change to the page or the relay reruns only
what that change moved plus it. `t test tilo-sh-local-vm` is the same stack
with its VM host, from the images that host runs.

## Changing code

```sh
os/containers/tilo-sh/stack.sh restart relay   # or door, web, tls, discord, releases
```

`restart` rebuilds the one service and starts it again, and leaves the rest
running with their state. For the relay, the door and the fake Discord it
runs the same `t build` as `up` (the same feature set each time, so a change
relinks only what it touches). For the web entry it rebuilds the desktop
bundle. A one-line change in the relay is running again in about two
minutes.

## What runs

Everything listens on `127.0.0.1`. Ports start at `TILO_SH_PORT_BASE`
(default 24100).

| Service | Production | Locally |
| --- | --- | --- |
| Lounge relay (room host and relay) | `eu1.tilo.sh` and `live-eu1.tilo.sh`, image `lounge` | `tilo-relay` through the image's entrypoint (`os/containers/tilo-lounge/entrypoint.sh`), laid out by the image's own context script, behind production's Caddy and Caddyfile at `https://127.0.0.1:24100` and `https://127.0.0.1:24106` |
| Front door (MCP, `/v1`, keys, OAuth) | `mcp.tilo.sh`, image `front-door` | `tilo-front-door` at `http://localhost:24110` |
| Web entry (site, `/sign-in`, `/install.sh`, `/releases`) | `tilo.sh`, image `web-entry` | the image's entrypoint and Caddyfile serving this checkout's desktop bundle, laid out and precompressed as the image does it, on `127.0.0.1:24121` behind the same Caddy, which serves it as Cloudflare serves tilo.sh (HTTPS and HTTP/2) at `https://tilo.localhost:24120`; `/releases/latest` serves this checkout's `tilo` command |
| Linux VM host (Firecracker) | `tilo-sh-vm-host`, image `vm-host` | the image as root (through `sudo`) where the machine has `/dev/kvm`: with `--images`, as part of the stack, and natively when `TILO_SH_VM_HOST=1` names the images it runs; otherwise skipped with the reason |
| Hosted browser | in the workspace's own VM, on the VM host | with the VM host: the relay places it there (docs/hosted-browser.md), and `t test tilo-sh-local-vm` holds it to what it must be |
| Discord | discord.com | `stack.sh up --discord` starts `crates/dev/fake-discord` and points the Lounge's Discord sign-in and bridge at it |

With `--images`, the first three are the images themselves.

Each service gets the environment its manifest sets in tilo/infra
(`apps/tilo-sh-relay`, `apps/tilo-front-door` and `apps/tilo-web`). Only the
public names change to loopback ports, and the secrets become throwaway
local ones. `stack.sh` lists the variables per service in `relay_env`,
`door_env` and `web_env`, and both modes use them. A change to a production
manifest belongs there too. Natively, each service also gets the `ENV` its
image's Containerfile sets, read from the Containerfile. Only the paths that
live in the image move under the stack's state, and an image path the stack
does not know how to move stops the start.

### What differs from production, and why

- **TLS.** In production, Cloudflare terminates TLS. Locally, Caddy's own
  local CA signs the relay's and the site's certificates. The front door and
  the relay get that CA in the trust bundle at the fixed path Tilo's binaries
  read. The door is plain HTTP on `http://localhost`, a development origin
  the door, the sign-in page and the `tilo` command already accept.
- **The site is HTTPS on its own name under localhost.** Caddy plays
  Cloudflare's edge in front of the web entry, so the page arrives over
  HTTP/2, as it does from tilo.sh; over plain HTTP a browser falls back to
  HTTP/1.1's six connections and the page's modules take a second longer at
  a 70 ms round trip, which skews any entry-speed measurement. The name is
  `tilo.localhost`, which browsers, curl and glibc resolve to loopback
  without any hosts entry, and which a browser treats as a secure context.
  It is not `localhost` itself, because a browser keeps the site's HSTS
  header per host name: on `localhost` it would then refuse the door's plain
  HTTP. It is not an address either, because a passkey's relying party is
  the page's host name and a browser refuses one on an address. So passkeys
  register and sign in here as on tilo.sh, for `tilo.localhost`.
- **Trusting the local CA.** `stack.sh browser` and the headless checks run
  Chromium with `--ignore-certificate-errors`, which also keeps the browser
  from recording HSTS. To use another browser profile, import
  `local-ca.pem` from the stack's state (`stack.sh env` prints its path as
  `TILO_TEST_CA_FILE`) as a trusted authority. `stack.sh down --wipe`
  deletes the CA, so a new one needs importing after it.
- **The live plane** is behind the same Caddy, at `https://127.0.0.1:24106`
  (production: `live-eu1.tilo.sh`). The relay's own endpoint uses it as its
  home relay. That endpoint trusts the local CA through the bundle, as every
  Tilo client does, iroh's packet-relay client included.
- **Natively, the build profile.** The programs are `lounge-gate` builds:
  the release's code and opt-level without the whole-program link, which
  is what the Lounge's own browser checks run. The image carries the
  release build. `--images` runs that.
- **Natively, the trust bundle.** Tilo's programs read one fixed bundle
  path, and no variable can redirect it. `bwrap` mounts the stack's bundle
  over that path for the relay and the door alone, as the containers mount
  it. Nothing else of the machine changes, and nothing runs as root.
- **Natively, Caddy's version.** The relay's TLS and the web entry use the
  `caddy` the caller built (`TILO_SH_CADDY`, which `t test e2e`
  hands the stack), else the `caddy` on your `PATH`, else the pinned release
  (`os/containers/tilo-sh/caddy.env`, the web entry image's version, one pin
  for the stack and the check that builds it) fetched once and checked
  against its published checksum. Production's tls container pins its own
  Caddy image.
- **Natively, compression.** The site is precompressed with the image's own
  script (`compress.sh`) when `brotli` and `zstd` are installed. Without
  them it is served uncompressed, and entry times are not comparable with
  production's. `TILO_SH_SITE` names a site laid out and precompressed
  already (`web-entry-context.sh`'s output, the web entry image's build
  context), which t's checks hand the stack instead of building one.
- **No model keys.** The moderation agent and the "who is here" summaries do
  not run.
- **The VM host is root.** In production it is a privileged pod: it formats a
  raw block device for its state, delegates cgroups and gives its jail helper
  capabilities. Rootless podman can do none of that, so the stack starts it as
  root over its own state (through `sudo`, where that needs no password), and
  only where the machine has `/dev/kvm`: with `--images`, or natively when
  `TILO_SH_VM_HOST=1` names its images. Its memory budget
  (`TILO_SH_VM_HOST_MEMORY_MIB`) holds the warm template of the size it places,
  a clone of it awake beside that, and the host's own reserve, since a host
  that admits a template and then refuses the VM it is for wakes workspaces
  cold. The stack also needs
  a member identity the relay operator certifies, which `vm-host.sh` makes
  from the relay's own root. Without `/dev/kvm` or root, `stack.sh` says it
  skipped the VM host and why; workspaces, keys, the MCP and the visa flow
  work without it, and Terminal's shell commands and a hosted workspace's
  Browser do not. `t test tilo-sh-local-vm` is that part: it runs on a node
  with KVM.

## Using it

```sh
eval "$(os/containers/tilo-sh/stack.sh env)"
```

This sets `TILO_DOOR`, `TILO_SITE`, and the local CA for Node and Python.
After that, everything points at the local stack:

- **A fresh visitor:** `stack.sh browser` opens Chromium on a throwaway
  profile at the local site. Headless, `integration/person.mjs SITE RELAY
  OUT` opens it as a fresh visitor and prints the time until they are in the
  Lounge.
- **The `tilo` command:** `tilo keys create "My backend"` prints a link.
  Approve it in the browser that `stack.sh browser` opened. The command
  prints the key. `--door` also works.
- **Python SDK:** `Tilo()` reads `TILO_DOOR` and `TILO_API_KEY`. For
  example: `uv run --project sdk/python python
  sdk/python/examples/visa_proof.py`.
- **TypeScript SDK:** `new Tilo()` reads the same two variables.
- **An agent's MCP client:** `claude mcp add --transport http tilo-local
  http://localhost:24110/mcp`. For a one-workspace key, use
  `http://localhost:24110/w/<workspace>/mcp` with `--header "Authorization:
  Bearer <token>"`.
- **Discord:** with `--discord`, the fake's control port
  (`http://127.0.0.1:24142`) posts and reads as Discord's users do. The
  routes are documented in `crates/dev/fake-discord/src/main.rs`.

`stack.sh status` shows what answers. `stack.sh logs relay` (or `tls`,
`door`, `web`, `discord`, `releases`, and `bundle` for the web bundle's
build) shows a service's log.
`stack.sh down` stops the stack, and `down --wipe` also deletes its state.

## Other builds

`TILO_SH_CARGO=<command>` builds the programs and the bundle's WebAssembly
with that cargo drop-in (run as `<command> build ARGS…`) instead of with
`t build`, for when `t` cannot bring a build back to your machine. It must
leave what it builds in `./target` as cargo would, the bundle's
`<target>/<profile>/<package>.wasm` included.

`TILO_SH_BUNDLE` names a desktop bundle built elsewhere
(`build.sh OUT --bundle desktop`) for the native stack to serve instead of
building one. `TILO_SH_PROGRAMS`, `TILO_SH_LOUNGE`, `TILO_SH_SITE`,
`TILO_SH_CADDY` and `TILO_SH_CLI_DIR` name what a check built already, so the
stack compiles and lays out nothing (see "Checking a change end to end").

With `--images`, `TILO_SH_IMAGE_LOUNGE`, `TILO_SH_IMAGE_DOOR` and `TILO_SH_IMAGE_WEB` each take an OCI archive or a registry reference.
To reproduce what is deployed, give the digests infra pins. To try a
release candidate, give its `sha-<commit>` images.

## The end-to-end check

```sh
os/containers/tilo-sh/stack.sh check   # this checkout's programs, brought up and used end to end
```

The check brings the stack up and uses it as people and backends do. `t test
e2e` runs the same steps against the stack it already has up, as flows beside
the page's (`check.sh --flow signin|visa|entry|passkey`), so this is what
`stack.sh check` does on its own:

1. A fresh startup identity, in its own headless Chromium, approves the
   server key that `tilo keys create` asks for on the local `/sign-in` page.
   What it reads there is kept as pictures in `startup/`: the sign-in card
   (the "In Browser" row included), the door's grants page, and the key that
   page makes, each light and dark. They also make a key on the door's
   grants page with "Operate web pages in Browser" unticked, which the last
   step holds to being asked about the page actions that act on a page. The
   page also makes a recovery key for that person, which is how the machine
   comes to hold them: `tilo-approve person` opens it into `startup/root`,
   and `tilo-approve next` answers what their hosts ask them, as one of their
   approving devices.
2. With that key, the visa run (`sdk/python/examples/visa_proof.py`) creates
   applicant workspaces (25 by default, set with `VISA_APPLICANTS`).
3. It mints a one-workspace key and its MCP URL.
4. The agent fills the form and writes the letter.
5. A reviewer, another person in their own browser
   (`integration/reviewer.py`), joins by the invitation, sees the filled
   form and who filled it, and undoes the agent's run.
6. Revoking the keys refuses them afterwards.
7. Three fresh visitors enter the Lounge, timed. One more keeps themselves
   with a passkey (a virtual authenticator) for `tilo.localhost`, the browser
   forgets the site, and the passkey signs the same person in again.

The report (`report.json` in the work directory) holds the bring-up time,
the visa run's duration and each visitor's entry time, and each step prints
its own seconds as the next one begins, so a run says where its time went.

To chase one step, name it: `TILO_SH_ONLY=7 stack.sh check` brings
the stack up and times the visitors, `TILO_SH_ONLY=2-6` runs only the visa
run (with the startup's key, which it needs), and `TILO_SH_ONLY=10,13-18` only
the Browser and the Linux commands of a workspace's VM (with the workspace the
open makes, which they need). Naming none runs every step.

### The Browser of a hosted workspace

`TILO_SH_VM_HOST=1 stack.sh check`, with `TILO_SH_IMAGE_VM_HOST` and
`TILO_SH_HOSTED_GUEST` naming the VM host's image and this checkout's guest,
adds the part that needs a machine with `/dev/kvm`: the stack's VM host runs,
so a workspace's Browser runs in that workspace's VM, on the guest image this
checkout built. (`t test tilo-sh-local-vm` is exactly this.) After the
startup's key it holds:

8. the visa test page (`apps/browser/test-pages/visa-form.html`), served
   where a VM's network may reach it: a VM reaches the public internet and
   one address of the machine it runs on, the relay's own endpoint at
   `10.0.2.4`, so the page is served on that endpoint's port.
9. the guest the VM host boots carries the Browser: the headless session
   (`/usr/libexec/tilo/browser-session`) and the image's unprivileged
   `tilo-browser` user. A guest without them fails here, saying so.
9b. the host warms the template its VMs of that size start from, timed: it is
    booted cold once and warmed, and every VM after it is a clone. A host
    without the room for a template beside the VM it is for makes none, and
    the wait says so rather than leaving the next step to boot cold.
10. a key opens Browser in a hosted workspace (`integration/hosted-visa.py`):
    the open answers once the VM's window is live, and the time it took is
    printed with a second open's attach time. The VM it woke must be a clone
    of the host's warm template; the wake the host measured is printed
    (`1234 ms (restored)`), and a VM that booted the guest image cold says
    why that is not what a person waits for.
11. the VM's own proofs (`integration/vm-proofs.py`, run while the window is
    up): every renderer of the engine host has a seccomp filter and no new
    privileges, and network, pid and user namespaces of its own; nothing runs
    the engine `--no-sandbox`; the session and the engine run as the image's
    browser user, whose device key no one else may read; the image's launcher
    entitlement lists name Browser; and what the session and its Chromium hold
    in memory, beside the VM's own, are printed and saved.
12. the agent fills the visa test page through page actions, with the person
    asked nothing (their key operates web pages in Browser) and the questions
    put to them held to none; captures it as a PNG; is refused one submission
    by the person, whose form stays filled and unsent; submits a second form
    instance with the person's one answer, naming the submission, and reads
    the page's receipt and reference; has a click on the submit button held
    and asked once; the key the person made with that scope unticked is
    asked about the page actions that act on the page (navigating and
    filling), which a key with the scope answers for; a reviewer who joined
    by invitation reads the run in
    History; and page actions past the plans file's free ones
    (`TILO_SH_BROWSER_FREE_ACTIONS`, 200 here: what the fills, the
    submissions and the refusals take) are refused with `no_payment_method`
    and the page a card is added on.

The same workspace's VM runs its Linux programs, and the steps after it hold
that half to what a customer gets (`integration/hosted-terminal.py`):

13. a Linux command in the workspace's VM, asked by an agent's run while the
    plans file names no card for the account, is refused at once with
    `no_payment_method`, the words that say it needs a Linux machine, and the
    page a card is added on — and nothing ran: the directory it would have
    made is not there when the next step looks.
14. the provider puts the account on file, as it would once a card is: the
    page action the free allowance refused a moment ago runs.
15. the workspace's Terminal, run as the person's own backend key: a file is
    made in the workspace's directory, listed and read back, `uname` answers,
    and `ls -d /usr/libexec/tilo/hosted-init` is there — a file only a
    workspace VM has, so the command ran in the VM and not on the machine
    holding it. The run is a structured record of the Terminal's session (the
    command, where it ran, when, how it ended, what it printed, who ran it)
    and a step of History named after the key that ran it, for the person.
16. an agent's key, minted for that one workspace, runs a command there
    through the `tilo` command (with `TILO_API_KEY`, the MCP behind it), and
    History keeps that run as the agent's, apart from the person's.
17. the Linux session runs as the VM's tenant, not as the account its Browser
    session runs as: `id` says `tilo` (uid 1000), the tenant cannot read the
    Browser user's home, and the Browser user cannot read a file the tenant
    made.
18. a Linux GUI app: the VM runs no display server, so a hosted workspace has
    Browser's window and no other, and there is no Linux app's window to open
    on the desk yet. The step holds the VM to that and fails, saying what to
    do, once one is wired: open it, see its window on the desk, close it.

Who answers the person: `tilo-approve` (a program this tree builds for the
check, like `tilo-enroll`) connects to the relay as one of the startup
person's approving devices, from the root their page's recovery key opened,
and answers the next question it is handed — allow, deny, or nothing at all,
which is how the check holds the filling to asking nothing. The question it
saw is printed and saved in `questions.json`: the action, what it acts on,
the agent that asked, the workspace and the risk.

The page shows only what it must: the capture, the page as the agent read it
back, the receipt, the questions the person was asked, the reviewer's History
and `vm-proofs.json` are in the work directory, and all of it is in the
report.

What it needs before it can hold:

- **A guest that carries the Browser.** Steps 9 onwards need a workspace
  guest with the headless session and the `tilo-browser` user in it, which is
  what the guest image is becoming; until then the check fails at step 9 and
  says exactly that.
- **Room for the stack.** Step 8 onwards run a workspace's VM beside the
  door, relay, web entry and VM host in the guest the check itself runs in.
  A workspace's VM runs at `default`, the size production places (4 vCPU,
  8 GiB, 50 GiB disk), and the VM host's budget is that size beside the
  host's own (10 GiB), so the placement is the one a customer's workspace
  gets. A machine with less room places smaller (`TILO_SH_HOSTED_VM_SIZE`),
  and `t` books the step's guest from what the step's runs measured, so a
  guest short of room says so and is given more next time.

## Entry-speed work

`TILO_SH_SEED=1 stack.sh check`, or `stack.sh up --seed` followed by
`integration/person.mjs`, times a fresh visitor against a Lounge that
production's members and history have filled. Compare `entry_ms` before and
after a change, on the same machine.
