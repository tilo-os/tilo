# Testing tilo.sh on your machine

tilo.sh is tested on a developer's machine or a build machine before a release,
never on the deployed site. One command starts the whole product from your
checkout, with the entrypoints, Caddyfiles and environment variables production
runs:

```sh
os/containers/tilo-sh/stack.sh up
```

By default the stack runs this checkout's programs and web bundle as plain
processes, built on a build server by a cargo drop-in in one command
(`os/containers/tilo-sh/build-on-builder.sh`), in this checkout's build
directory there, which stays from one bring-up to the next: every program in
one incremental build of the release's code without its whole-program link
(Cargo's `lounge-local`), and beside it the desktop bundle's module
(`build.sh --profile local`: the same for the bundle, bound and split as the
shipped bundle is, without wasm-opt). A change recompiles the parts of the
crates it touched; a module that did not change is neither bound nor split
again; only the files that changed come back. Nothing but the fetch, the
relay's and the site's layout and the site's precompression (at faster levels
than the image's, each file compressed once per content) runs on your
machine. Each service gets its own state and runtime directory under the
stack's state. `stack.sh help` lists every command.

`TILO_SH_BUILD=t` takes the programs and the bundle from t's checks instead:
`tilo-sh-programs` (`lounge-gate`) and `web-desktop-gate-bundle` (compiled,
bound, split and wasm-opt'd on t), the builds the checks run. They start from
an empty build directory, so they take minutes where the default takes
seconds; use them to rehearse exactly what the checks build. A clean checkout
is built as its commit, one with changes as t finds it when the run starts.

A fresh Lounge takes people by invitation, as production's does, and nobody
owns it yet: `up` prints an owner link, and whoever opens it, in any browser,
is in as the Lounge's owner (`stack.sh owner` prints it again).

`stack.sh up --images` runs the release images instead: `image-lounge`,
`image-api` and `image-web-entry`, built by `t` for this commit, under rootless
podman. That rehearses what ships, and the images' own checks hold them to
booting, answering their health route and answering one request per route.

Each service gets the environment its manifest sets in tilo/infra
(`apps/tilo-sh-relay`, `apps/tilo-api` and `apps/tilo-web`); only the public
names change to loopback ports and the secrets become throwaway local ones.
`stack.sh` lists the variables per service in `relay_env`, `api_env` and
`web_env`, and both modes use them, so a change to a production manifest
belongs there too. Natively each service also gets the `ENV` its image's
Containerfile sets. Only paths that live in the image move under the stack's
state; an image path the stack does not know how to move stops the start.

## What runs

Everything listens on `127.0.0.1`. Ports start at `TILO_SH_PORT_BASE`
(default 24100).

| Service | Production | Locally |
| --- | --- | --- |
| Lounge relay | `eu1.tilo.sh` and `live-eu1.tilo.sh`, image `lounge` | `tilo-relay` through the image's entrypoint, laid out by the image's context script, behind production's Caddy at `127.0.0.1:24100` and `127.0.0.1:24106`; its door takes people by invitation and keeps a wait list (`TILO_SH_ADMISSION=open` admits anyone as a guest, which the checks ask for) |
| Mail (wait-list confirmations, invitations) | Resend | a sink that keeps each mail as a file at `127.0.0.1:24123`; `stack.sh mail` lists them with their links |
| API server (MCP, `/v1`, keys, OAuth) | `mcp.tilo.sh`, image `api` | `tilo-api` at `http://localhost:24110` |
| Web entry (site, `/sign-in`, `/install.sh`, `/releases`) | `tilo.sh`, image `web-entry` | the image's entrypoint and Caddyfile serving this checkout's desktop bundle, laid out and precompressed as the image does it, on `127.0.0.1:24121` behind the same Caddy at `http://tilo.localhost:24120`; `/releases/latest` serves this checkout's `tilo` command, signed with the stack's own key, which the site's installer downloads and trusts instead of Tilo's release key |
| Linux VM host (Firecracker) | `tilo-sh-vm-host`, image `vm-host` | the image as root through `sudo` where the machine has `/dev/kvm`: with `--images`, or natively when `TILO_SH_VM_HOST=1` names its images; otherwise skipped with the reason |
| Hosted browser | in the workspace's own VM, on the VM host | with the VM host: the relay places it there ([hosted Browser](hosted-browser.md)) |
| Discord | discord.com | `stack.sh up --discord` starts `crates/dev/fake-discord` and points the Lounge's Discord sign-in and bridge at it |

With `--images`, the first three are the images themselves.

### What differs from production

- **No TLS.** In production Cloudflare terminates TLS; here the site, the relay,
  its live plane and the API server are plain HTTP on this machine's own names.
  Browsers treat `localhost`, names under it and loopback addresses as secure
  contexts, so the page runs as it does on tilo.sh (passkeys, WebCrypto, service
  workers) with no certificate to trust. Tilo's programs and the `tilo` command
  take `https://` origins, or `http://` ones on this machine.
- **WebTransport** needs TLS, so the relay serves it on its own UDP port with a
  short-lived ECDSA certificate it makes and whose SHA-256 the page is handed
  with the host's address (`serverCertificateHashes`).
- **The site is on its own name under localhost.** `tilo.localhost` resolves to
  loopback without a hosts entry, and it is a name rather than an address
  because a passkey's relying party is the page's host name. Over plain HTTP a
  browser speaks HTTP/1.1, so the page's modules arrive over six connections and
  an entry-speed measurement here reads slower than tilo.sh.
- **Natively, the build profile** is `lounge-local`: the release's code and
  opt-level without the whole-program link, compiled incrementally. The
  bundle is not wasm-opt'd, so its modules are larger than the shipped ones.
  The image carries the release build.
- **Natively, the trust bundle:** Tilo's programs read one fixed bundle path and
  no variable redirects it; `bwrap` mounts this machine's roots over that path
  for the relay and the API server alone.
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

This sets `TILO_API_URL` and `TILO_SITE`, and everything points at the local
stack:

- **You, the owner:** open the owner link `up` printed (`stack.sh owner` prints
  it again) in your browser. It makes your identity on the spot and you are in
  the Lounge as its owner and an administrator.
- **Someone you invite:** `stack.sh invite [WORKSPACE] -- --role editor` prints
  an invite link from the stack's host (the card names the workspace). Opened
  in a browser with no Tilo identity yet, the page makes one and joins; being
  in any workspace here also lets them past the invite-only sign-up and into
  the Lounge.
- **A fresh visitor:** `stack.sh browser` opens Chromium on a throwaway profile
  at the local site. `integration/person.mjs SITE RELAY OUT` does the same
  headless and prints the time until they are in the Lounge.
- **The `tilo` command:** `tilo keys create "My backend"` prints a link.
  Approve it in the browser `stack.sh browser` opened, and the command prints
  the key.
- **Python SDK:** `Tilo()` reads `TILO_API_URL` and `TILO_API_KEY`, e.g.
  `uv run --project sdk/python python sdk/python/examples/visa_proof.py`.
- **TypeScript SDK:** `new Tilo()` reads the same two variables.
- **An agent's MCP client:** `claude mcp add --transport http tilo-local
  http://localhost:24110/mcp`. For a one-workspace key use
  `http://localhost:24110/w/<workspace>/mcp` with `--header "Authorization:
  Bearer <token>"`.
- **Discord:** with `--discord`, the fake's control port at
  `http://127.0.0.1:24142` posts and reads as Discord's users do.

`stack.sh status` shows what answers, `stack.sh logs relay` (or `tls`, `api`,
`web`, `discord`, `releases`, `bundle`) shows a service's log, `stack.sh down`
stops the stack and `down --wipe` deletes its state.

## The end-to-end check

```sh
os/containers/tilo-sh/stack.sh check
```

The check brings the stack up and uses it as people and backends do: a fresh
identity approves the server key `tilo keys create` asks for on `/sign-in`; the
visa run (`sdk/python/examples/visa_proof.py`) creates applicant workspaces,
mints a one-workspace key and its MCP URL, and an agent fills a form and writes
a letter; a reviewer joins by invitation, reads the filled form and undoes the
agent's run; revoking the keys refuses them; three fresh visitors enter the
Lounge, one of them with a passkey. With `TILO_SH_VM_HOST=1` it also holds a
hosted workspace's Browser to opening in that workspace's VM and to the
sandbox, browser user and device key the guest image promises, fills the test
page through page actions with a person's answers, and runs Linux commands in
the workspace's own VM as its tenant rather than as its Browser user.

`t test e2e` runs the same steps against a stack it already has up, as flows
beside the page's own (`check.sh --flow signin|visa|entry|passkey`).
`t test tilo-sh-local-vm` runs the check with the VM host, from the images that
host runs. The report in the work directory holds the bring-up time, the visa
run's duration and each visitor's entry time, so a run says where its time
went.

`TILO_SH_SEED=1 stack.sh check`, or `stack.sh up --seed` followed by
`integration/person.mjs`, times a fresh visitor against a Lounge filled by
production's members and history. Compare `entry_ms` before and after a change
on the same machine.

## Other builds

`TILO_SH_CARGO=<command>` builds with that cargo drop-in instead of the one
`$CARGO` names, else `cargo` on this machine. It must leave what it builds in
`./target` as cargo would. A drop-in that also
runs a program on its builder and brings files of its target dir back
(`<command> --fetch PATH exec PROGRAM ARGS…`) gets the builds in one command
there; with any other (`cargo`), the bundle's build script
binds and splits here and needs wasm-bindgen.

`TILO_SH_BUNDLE` names a desktop bundle built elsewhere (`build.sh OUT --bundle
desktop`) for the native stack to serve. `TILO_SH_PROGRAMS`, `TILO_SH_LOUNGE`,
`TILO_SH_SITE`, `TILO_SH_CADDY` and `TILO_SH_CLI_DIR` name what a check built
already, so the stack compiles and lays out nothing. With `--images`,
`TILO_SH_IMAGE_LOUNGE`, `TILO_SH_IMAGE_API` and `TILO_SH_IMAGE_WEB` each take an
OCI archive or a registry reference; a release candidate's `sha-<commit>`
images reproduce what is deployed.

## Releasing the Lounge's people

A release starts the Lounge on fresh state: it does not read the previous
release's people, roles, wait list or invitations back. The deployment brings
them over once, in the relay's own pod (tilo/infra `apps/tilo-sh-relay`), with
nobody reaching the cluster. `t test release-tilo-sh` proves the same steps on
the previous release's own state (`relay-upgrade`).

1. **Dispatch the release**: `t dispatch release-tilo-sh --ref main --repo tilo
   --publish`, and keep the three image digests it prints (the signing steps are
   skipped here; they run on a trusted release run).
2. **Push infra**: the branch that starts the host on a new state directory
   (`TILO_WORKSPACES_STATE=/run/tilo-rooms/<version>`), with the new digests,
   and points the deployment's carry-over at the old and new state (the
   `release-people` container's `OLD_STATE` and `NEW_STATE`, the
   `keep-identity` init container's paths). The old release's files stay where
   they are.
3. **The host keeps its identity**: before the host first serves from the new
   directory, `keep-identity` copies the old release's device key
   (`relay/host-device.key`) into it, once. The endpoint the VM hosts take work
   from and the address a workspace's Browser reaches the host at are derived
   from that key, so they stay the same.
4. **The people come over**: the `release-people` container waits for the new
   Lounge's operator socket (`<new state>/lounge/operator.sock`), then runs

   ```sh
   tilo-relay release-people export --state <old state> --out <new state>/release-people/people.json
   tilo-relay release-people import --socket <new state>/lounge/operator.sock \
     --file <new state>/release-people/people.json
   ```

   The export reads the old release's state and writes nothing there: the
   addresses it kept on its wait list, with the secrets their removal and
   confirmation links carry, and the people with the role each had (an
   administrator is an administrator, the workspace's own root is nobody). The
   import brings each address back as it stood, declares an owner by their key,
   and gives every other person the host's own invitation, mailed to the
   address the export names. Once the host has answered, the container writes
   `<new state>/release-people/imported` and never imports again, even after a
   restart, so nobody is invited twice.
5. **Hand out the links nobody was mailed**: the import prints every invitation
   with its link, `, mailed` after the ones the host sent. The others reach
   their people only through whoever runs the host. They are in the
   container's log, and kept on the volume in
   `<new state>/release-people/import.log`:

   ```sh
   kubectl -n tilo-sh-vm-host logs deploy/tilo-sh-relay -c release-people
   ```

6. **Check**: the import named every person and the link each one got, the
   addresses it mailed were the ones the previous release kept, the wait list
   lists the same addresses (an unconfirmed one still waits for its link to be
   opened), the administrators stand as administrators, and the previous
   release's files are still where it left them.
