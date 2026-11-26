# Testing tilo.sh on your machine

tilo.sh is tested on a developer's machine or a build machine before a release,
never on the deployed site. One command starts the whole product from your
checkout, with the entrypoints, Caddyfiles and environment variables production
runs:

```sh
os/containers/tilo-sh/stack.sh up
```

By default the whole stack runs **on the build server**, in this checkout's
build container, with the ports a person and their tools use served from there
on this machine's loopback under the same numbers
(`$CARGO exec --forward`, see [Where it runs](#where-it-runs)). The site opens
at the same `http://tilo.localhost:<base+20>`, the API server answers at the
same `http://localhost:<base+10>`, and `tilo login`, the SDKs and an agent's
MCP client reach it as they reach a stack here. Everything else — the
programs, every service, the state, the build — stays on the build server,
under a per-stack directory of this checkout's build directory there
(`target/tilo-stack/<port base>`), and `stack.sh down --wipe` deletes it there.
That is what lets many stacks run at once without loading the machine you are
sitting at: SSH carries the forwarded ports and one long-running command, and
the stack holds the build server's memory admission while it runs.

The programs and the web bundle are this checkout's. The programs are built in
the build container there: every program in one incremental build of the
release's code without its whole-program link (Cargo's `lounge-local`). The web
bundle is t's `web-entry-bundle` check, the one the web entry's image ships
(compiled, bound, split and wasm-opt'd on t), fetched with `t outputs`, so the
stack serves the bundle the check defines. A change recompiles the parts of the
crates it touched; what did not change is kept, and `stack.sh restart NAME`
rebuilds and restarts one service while the others keep running. The server lets
the turns its agents are taking end before it stops, so a restart never cuts an
answer off; `restart --detach NAME...` builds first, says whether that worked,
and leaves the restarts to follow
on their own, which is how an agent restarts the stack it talks through from its
own Terminal. Each service gets its own state and runtime directory under the
stack's state. `stack.sh help` lists every command.

Without a VM for each workspace, a stack starts no programs in their Terminal
beyond the shell's own commands. `TILO_SH_TERMINAL_ON_MACHINE=1` has it start them on
the machine itself, as the stack's user with the `PATH` it was brought up with:
for a stack that is one person's own machine, such as a development VM whose
agent edits and rebuilds the stack. Nothing confines those programs. That
Terminal also carries the stack's own settings (its state, port base, build tool
and model), so `stack.sh restart --detach server web` typed there acts on this
stack.

The server keeps no sealed store of its own: every sign-in, provider key and
vault item is an entry of its workspace's vault, the document Passwords keeps,
and `stack.sh vaults` fails where a store of the server's own lies in its
state. `stack.sh` hands a service only variables its image sets, and
stops the start at one the image does not set instead of leaving it out.

`TILO_SH_REMOTE=0` runs everything here instead, which is what the end-to-end
check (`stack.sh check`), `up --images` and the VM host need. A checkout whose
cargo drop-in cannot forward ports (a drop-in from before `exec --forward`),
a stack handed what a check built (`TILO_SH_PROGRAMS` and the like), and
`TILO_SH_BUILD=t` all do the same, and `up` says why.

`TILO_SH_BUILD=t` takes the programs from t's check instead:
`tilo-sh-programs` (`lounge-check`), the build the check runs. It starts from
an empty build directory, so it takes minutes where the default takes
seconds; use it to rehearse exactly what the check builds. A clean checkout
is built as its commit, one with changes as t finds it when the run starts.
(The web bundle is always t's `web-entry-bundle`, as above.)

A fresh Lounge takes people by invitation, as production's does. The stack
makes a person of its own and starts the server with them as the Lounge's owner,
and `up` prints an administrator link they made: whoever opens it, in any
browser, is in as an administrator (`stack.sh owner` prints it again).

`stack.sh up --images` runs the release images instead: `image-lounge`,
`image-api` and `image-web-entry`, built by `t` for this commit, under rootless
podman. That rehearses what ships, and the images' own checks hold them to
booting, answering their health route and answering one request per route.

Each service gets the environment its manifest sets in tilo/infra
(`apps/tilo-sh-server`, `apps/tilo-api` and `apps/tilo-web`); only the public
names change to loopback ports and the secrets become throwaway local ones.
`stack.sh` lists the variables per service in `server_env`, `api_env` and
`web_env`, and both modes use them, so a change to a production manifest
belongs there too. Natively each service also gets the `ENV` its image's
Containerfile sets. Only paths that live in the image move under the stack's
state; an image path the stack does not know how to move stops the start.

## Where it runs

Two machines: the one you are at, and the build server. The stack itself is
whole on the build server; this machine carries only these ports, forwarded
under their own numbers:

| Port | What |
| --- | --- |
| `BASE+20` | the site, `http://tilo.localhost:BASE+20` |
| `BASE+10` | the API server (MCP, `/v1`, keys, OAuth) |
| `BASE` | the server's public origin (which the page dials) |
| `BASE+6` | the live origin |
| `BASE+5` | the server's own web listener (HTTPS) |
| `BASE+21`, `BASE+22`, `BASE+23` | the web entry, the releases, the mail sink |
| `BASE+40`…`BASE+42` | the fake Discord (`up --discord`) |
| `BASE+2`, `BASE+4`, `BASE+5` (UDP) | the page's QUIC: the packet relay, iroh, and WebTransport to the server's own web listener |

A page reaches its relay over QUIC, which SSH cannot carry, so the drop-in
carries UDP through the build server's own helper: one channel per client that
sends anything, each datagram framed. The forwards end with the stack's
bring-up process; `stack.sh status` says whether they are up.

`up` refuses a base whose ports already answer, here or on the build server,
naming what holds them (a second worktree's stack, or a forward left from
one). It also refuses a base whose ports reach into the range the machine
hands to outgoing connections (`32768` and up on many machines; read from
`/proc/sys/net/ipv4/ip_local_port_range` there), naming a safe one: such a
port can be taken from the stack at any moment, and a service that cannot bind
it fails while the stack looks up. A remote bring-up then checks that what
answers through the forward is its own stack, by a token only its releases
serve, so another stack on the same base cannot pass for it.

The commands that act on the stack — `owner`, `invite`, `tilo`, `hold`,
`mail`, `mail-account`, `logs`, `restart` — run where the stack is, so the
owner and the invites stay in one place. `browser` opens the page here, on a
fresh profile.

## What runs

Everything listens on `127.0.0.1`. Ports start at `TILO_SH_PORT_BASE`
(default 24100).

| Service | Production | Locally |
| --- | --- | --- |
| Lounge server | `eu1.tilo.sh` and `live-eu1.tilo.sh`, image `lounge` | `tilo-server` through the image's entrypoint, laid out by the image's context script, behind production's Caddy at `127.0.0.1:24100` and `127.0.0.1:24106`; its entry takes people by invitation and keeps a wait list (`TILO_SH_ADMISSION=open` admits anyone as a guest, which the checks ask for) |
| Mail (wait-list confirmations, invitations) | Resend | a sink that keeps each mail as a file at `127.0.0.1:24123`; `stack.sh mail` lists them with their links |
| A mail account's servers | the person's mail provider | with `TILO_SH_MAIL_STAND_IN=1`, an IMAP and an SMTP server with real sockets at `127.0.0.1:24124` and `127.0.0.1:24125` that take one made-up login, show an empty inbox and send nothing; its certificate is in this stack's trust bundle alone, and `stack.sh mail-account` prints the settings to type |
| API server (MCP, `/v1`, keys, OAuth) | `mcp.tilo.sh`, image `api` | `tilo-api` at `http://localhost:24110` |
| Web entry (site, `/sign-in`, `/install.sh`, `/releases`) | `tilo.sh`, image `web-entry` | the image's entrypoint and Caddyfile serving this checkout's entry bundle, laid out and precompressed as the image does it, on `127.0.0.1:24121` behind the same Caddy at `http://tilo.localhost:24120`; `/releases/latest` serves this checkout's `tilo` command, signed with the stack's own key, which the site's installer downloads and trusts instead of Tilo's release key |
| Linux VM host (Firecracker) | `tilo-sh-vm-host`, image `vm-host` | the image as root through `sudo` where the machine has `/dev/kvm`: with `--images`, or natively when `TILO_SH_VM_HOST=1` names its images; otherwise skipped with the reason |
| Hosted browser | in the workspace's own VM, on the VM host | with the VM host: the server places it there ([hosted Browser](hosted-browser.md)) |
| Discord | discord.com | `stack.sh up --discord` starts `tools/fake-discord` and points the Lounge's Discord sign-in and bridge at it |

With `--images`, the first three are the images themselves.

### What differs from production

- **No TLS.** In production Cloudflare terminates TLS; here the site, the server,
  its live session and the API server are plain HTTP on this machine's own names.
  Browsers treat `localhost`, names under it and loopback addresses as secure
  contexts, so the page runs as it does on tilo.sh (WebCrypto, service workers)
  with no certificate to trust. Tilo's programs and the `tilo` command
  take `https://` origins, or `http://` ones on this machine.
- **WebTransport** needs TLS, so the relay serves it on its own UDP port with a
  short-lived ECDSA certificate it makes and whose SHA-256 the page is handed
  with the host's address (`serverCertificateHashes`).
- **The site is on its own name under localhost.** `tilo.localhost` resolves to
  loopback without a hosts entry, and it is a name rather than an address
  because a browser gives its secure context, and WebCrypto, only to a host
  name. Over plain HTTP a browser speaks HTTP/1.1, so the page's modules arrive
  over six connections and an entry-speed measurement here reads slower than
  tilo.sh.
- **Natively, the programs' build profile** is `lounge-local`: the release's
  code and opt-level without the whole-program link, compiled incrementally.
  The web bundle is t's `web-entry-bundle`, the shipped one, wasm-opt'd and
  split as the image serves it.
  The image carries the release build.
- **Natively, the trust bundle:** Tilo's programs read one fixed bundle path and
  no variable redirects it; `bwrap` mounts this machine's roots over that path
  for the server and the API server alone.
- **Natively, Caddy** is the one the caller built (`TILO_SH_CADDY`, which
  `t test e2e` hands the stack), else the `caddy` on your `PATH`, else the pinned
  release in `caddy.env`, fetched once and checked against its published
  checksum.
- **Natively, compression** needs `brotli` and `zstd`; without them the site is
  served uncompressed and entry times are not comparable with production's.
  `TILO_SH_SITE` names a site already laid out and precompressed.
- **No model keys:** the moderation agent and the "who is here" summaries do not
  run.
- **The VM host is root.** In production it is a privileged pod that formats a
  raw block device for its state, delegates cgroups and gives its jail helper
  capabilities; rootless podman can do none of that. Without `/dev/kvm` or root,
  `stack.sh` says it skipped the VM host. Workspaces, keys, the MCP and the visa
  flow work without it; Terminal's shell commands and a hosted workspace's
  Browser do not.

## Using it

```sh
eval "$(os/containers/tilo-sh/stack.sh env)"
```

This sets `TILO_API_URL` and `TILO_SITE`, and the SDKs and agents point at the
local stack:

- **You, an administrator:** open the administrator link `up` printed
  (`stack.sh owner` prints it again) in your browser. It makes your identity on
  the spot and you are in the Lounge as an administrator. The stack's own person, the
  owner it started the Lounge with, needs no browser:
  `eval "$(stack.sh owner --here)"` gives you a key for them, and
  `stack.sh tilo do workspace/waiting#list -w Lounge` runs `tilo` as them.
- **Someone you invite:** `stack.sh invite [WORKSPACE] -- --role editor` prints
  an invite link the stack's owner makes with `tilo do workspace/invitations#invite` (the card
  names the workspace). Opened in a browser with no Tilo identity yet, the page
  makes one and joins; being in any workspace here also lets them past the
  invite-only sign-up and into the Lounge.
- **A fresh visitor:** `stack.sh browser` opens Chromium on a throwaway profile
  at the local site. `integration/person.mjs SITE SERVER OUT` does the same
  headless and prints the time until they are in the Lounge.
- **The `tilo` command:** `tilo login` makes this computer one of your devices;
  `tilo do account/keys#create name="My backend" --out my-backend.key` then prints
  a link. Approve it in the browser `stack.sh browser` opened, and the file has
  the key.
- **Python SDK:** `Tilo()` reads `TILO_API_URL` and `TILO_API_KEY`, e.g.
  `uv run --project sdk/python python sdk/python/examples/visa_proof.py`.
- **TypeScript SDK:** `new Tilo()` reads the same two variables.
- **An agent's MCP client:** the streamable HTTP server
  `http://localhost:24110/mcp`. For a one-workspace key use
  `http://localhost:24110/w/<workspace>/mcp` with the header `Authorization:
  Bearer <token>`.
- **Discord:** with `--discord`, the fake's control port at
  `http://127.0.0.1:24142` posts and reads as Discord's users do.

`stack.sh status` shows what answers, `stack.sh logs server` (or `tls`, `api`,
`web`, `discord`, `releases`, `bundle`) shows a service's log, `stack.sh down`
stops the stack and `down --wipe` deletes its state.

## The end-to-end check

```sh
os/containers/tilo-sh/stack.sh check
```

The check brings the stack up and uses it as people and backends do: a fresh
identity approves the server key `tilo do account/keys#create` asks for on `/sign-in`; the
visa run (`sdk/python/examples/visa_proof.py`) creates applicant workspaces,
mints a one-workspace key and its MCP URL, and an agent fills a form and writes
a letter; a reviewer joins by invitation, reads the filled form and undoes the
agent's run; revoking the keys refuses them; three fresh visitors enter the
Lounge. With `TILO_SH_VM_HOST=1` it also holds a
hosted workspace's Browser to opening in that workspace's VM and to the
sandbox, browser user and device key the guest image promises, fills the test
page through page actions with a person's answers, and runs Linux commands in
the workspace's own VM as its tenant rather than as its Browser user.

`t test e2e` runs the same steps against a stack it already has up, as flows
beside the page's own (`check.sh --flow signin|visa|entry`).
`t test tilo-sh-local-vm` runs the check with the VM host, from the images that
host runs. The report in the work directory holds the bring-up time, the visa
run's duration and each visitor's entry time, so it says where the time went.

`t test tilo-sh-answers-a-program` brings a stack up from `tilo-sh-programs`'
build with a stub site and sends the API server one small tilolang program with
the key of the stack's own person (`stack.sh owner --here`). The answer must be
`ok`, which it is only where `tilolang-process` lies beside `tilo-api`: the API
server starts a task's programs in it (`os/containers/tilo-sh/test-program.sh`).

`TILO_SH_SEED=1 stack.sh check`, or `stack.sh up --seed` followed by
`integration/person.mjs`, times a fresh visitor against a Lounge filled by
production's members and history. Compare `entry_ms` before and after a change
on the same machine.

## Other builds

`TILO_SH_CARGO=<command>` builds the programs with that cargo drop-in instead of
the one `$CARGO` names, else `cargo` on this machine. It must leave what it
builds in `./target` as cargo would. A drop-in that also
runs a program on its builder and brings files of its target dir back
(`<command> --fetch PATH exec PROGRAM ARGS…`) builds them in one command there.

`TILO_SH_BUNDLE` names a bundle built elsewhere (`build.sh OUT` or t's
`web-entry-bundle` outputs) for the native stack to serve. `TILO_SH_PROGRAMS`,
`TILO_SH_LOUNGE`, `TILO_SH_SITE`, `TILO_SH_CADDY` and `TILO_SH_CLI_DIR` name what
a check built already, so the stack compiles and lays out nothing. With
`--images`,
`TILO_SH_IMAGE_LOUNGE`, `TILO_SH_IMAGE_API` and `TILO_SH_IMAGE_WEB` each take an
OCI archive or a registry reference; a release candidate's `sha-<commit>`
images reproduce what is deployed.

## Releasing

A commit on main that passes what this page describes is released with one command, run on the
build server from a checkout of this repository:

```
tools/release/tilo-sh.sh --infra ~/work/infra
```

It has t publish main's head (`t dispatch release-tilo-sh`: the Lounge's server, the web entry
and the API server built from that one commit, published as `dev-<commit>` once `e2e`,
`onboarding` and `tilo-sh-admits-a-page` pass against the same build), writes the three digests
into every pin of them in the tilo/infra checkout, and starts production's pods on this machine
before pushing (`tools/release/tilo_sh_preflight.py`): the pins tilo/infra has committed on a
scratch volume, then the release on that same volume, as Argo's rollout replaces them, with
rootless podman and no network. The preflight refuses a manifest that sets a `TILO_*` variable
no file of its image mentions (a setting the build renamed or dropped), a pod that does not
answer its readiness, a web entry whose `TILO_WEB_HOME_HOST_INFO` is not in the form the new
server prints, and a Lounge whose device key or workspace changed across the upgrade. With
`--state DIR`, a read-only copy of the live relay's volume, the release starts on that instead,
and the host the web entry names must be the one the state holds.

Then it makes one commit on tilo/infra's main with the pins and whatever else the checkout
changes for this release (a setting the new build reads under another name goes in the same
commit as the image that reads it), pushes it, and waits until `https://eu1.tilo.sh/mode` and
`https://tilo.sh/version.json` report the release's commit. Each step prints how long it took.
`--no-push` stops before the commit; `--images LOUNGE WEB API` pins images an earlier run
published instead of publishing again. Going back is reverting that commit.
