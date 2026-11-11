# Testing tilo.sh on your machine

tilo.sh is tested on a developer's machine (or the rcargo builder) before any
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
under rootless podman. That is the rehearsal of what ships, and it is what
`t test tilo-sh-local` runs. The first run on a commit waits for the image
builds (about 40 minutes).

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
| Web entry (site, `/sign-in`, `/install.sh`, `/releases`) | `tilo.sh`, image `web-entry` | the image's entrypoint and Caddyfile serving this checkout's desktop bundle, laid out and precompressed as the image does it, at `http://localhost:24120`; `/releases/latest` serves this checkout's `tilo` command |
| Linux VM host (Firecracker) | `tilo-sh-vm-host`, image `vm-host` | with `--images`, the image as root (through `sudo`) where the machine has `/dev/kvm`; otherwise, and always natively, skipped with the reason |
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
  local CA signs the relay's certificate. The front door and the relay get
  that CA in the trust bundle at the fixed path Tilo's binaries read. The
  door and the site are plain HTTP on `http://localhost`. That is a
  development origin the door, the sign-in page and the `tilo` command
  already accept, so clients need no CA to reach them.
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
  `caddy` on your `PATH`, else Caddy 2.11.7 (the web entry image's version),
  fetched once and checked against its release checksum. Production's tls
  container pins its own Caddy image.
- **Natively, compression.** The site is precompressed with the image's own
  script (`compress.sh`) when `brotli` and `zstd` are installed. Without
  them it is served uncompressed, and entry times are not comparable with
  production's.
- **No model keys.** The moderation agent and the "who is here" summaries do
  not run.
- **The VM host is root.** In production it is a privileged pod: it formats a
  raw block device for its state, delegates cgroups and gives its jail helper
  capabilities. Rootless podman can do none of that, so `up --images` starts
  it as root over its own state (through `sudo`, where that needs no
  password), and only where the machine has `/dev/kvm`. The stack also needs
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

`TILO_SH_CARGO=rcargo` builds the programs and the bundle's WebAssembly on
rcargo's builder instead of with `t build`, for when `t` cannot bring a
build back to your machine.

`TILO_SH_BUNDLE` names a desktop bundle built elsewhere
(`build.sh OUT --bundle desktop`) for the native stack to serve instead of
building one.

With `--images`, `TILO_SH_IMAGE_LOUNGE`, `TILO_SH_IMAGE_DOOR` and `TILO_SH_IMAGE_WEB` each take an OCI archive or a registry reference.
To reproduce what is deployed, give the digests infra pins. To try a
release candidate, give its `sha-<commit>` images.

## The end-to-end check

```sh
os/containers/tilo-sh/stack.sh check            # this checkout's programs
os/containers/tilo-sh/stack.sh check --images   # the images, as t test tilo-sh-local runs it
```

The check brings the stack up and uses it as people and backends do:

1. A fresh startup identity, in its own headless Chromium, approves the
   server key that `tilo keys create` asks for on the local `/sign-in` page.
2. With that key, the visa run (`sdk/python/examples/visa_proof.py`) creates
   applicant workspaces (25 by default, set with `VISA_APPLICANTS`).
3. It mints a one-workspace key and its MCP URL.
4. The agent fills the form and writes the letter.
5. A reviewer, another person in their own browser
   (`integration/reviewer.py`), joins by the invitation, sees the filled
   form and who filled it, and undoes the agent's run.
6. Revoking the keys refuses them afterwards.
7. Three fresh visitors enter the Lounge, timed.

The report (`report.json` in the work directory) holds the bring-up time,
the visa run's duration and each visitor's entry time.

### The Browser of a hosted workspace

`TILO_SH_VM_HOST=1 stack.sh check --images` adds the part that needs a
machine with `/dev/kvm`: the stack's VM host runs, so a workspace's Browser
runs in that workspace's VM, on the guest image this checkout built.
(`t test tilo-sh-local-vm` is exactly this.) After the startup's key it holds:

8. the visa test page (`apps/browser/test-pages/visa-form.html`), served
   where a VM's network may reach it: a VM reaches the public internet and
   one address of the machine it runs on, the relay's own endpoint at
   `10.0.2.4`, so the page is served on that endpoint's port.
9. the guest the VM host boots carries the Browser: the headless session
   (`/usr/libexec/tilo/browser-session`) and the image's unprivileged
   `tilo-browser` user. A guest without them fails here, saying so.
10. a key opens Browser in a hosted workspace (`integration/hosted-visa.py`):
    the open answers once the VM's window is live, and the time it took is
    printed with a second open's attach time.
11. the VM's own proofs (`integration/vm-proofs.py`, run while the window is
    up): every renderer of the engine host has a seccomp filter and no new
    privileges, and network, pid and user namespaces of its own; nothing runs
    the engine `--no-sandbox`; the session and the engine run as the image's
    browser user, whose device key no one else may read; the image's launcher
    entitlement lists name Browser; and what the session and its Chromium hold
    in memory, beside the VM's own, are printed and saved.
12. the agent fills the visa test page through page actions, captures it as a
    PNG and stops before submitting; a reviewer who joined by invitation reads
    its run in History; and page actions past the plans file's free ones
    (`TILO_SH_BROWSER_FREE_ACTIONS`, 40 here: what an agent filling a form
    takes) are refused until the account is added to it, which the check does
    as the provider would.

The page shows only what it must: the capture, the page as the agent read it
back, the reviewer's History and `vm-proofs.json` are in the work directory,
and all of it is in the report.

## Entry-speed work

`TILO_SH_SEED=1 stack.sh check`, or `stack.sh up --seed` followed by
`integration/person.mjs`, times a fresh visitor against a Lounge that
production's members and history have filled. Compare `entry_ms` before and
after a change, on the same machine.
