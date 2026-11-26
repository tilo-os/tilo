# Tilo Lounge

One always-on public Tilo workspace, hosted by the device's one daemon: the
workspace is placed on the server and its pages reach it through the web entry.

A machine becomes the Lounge as any computer joins a person's devices: a ticket
from `tilo device add`, spent once on the volume. Everything about the workspace
itself — who is in it, its listing, its chat channels, its bots — is an action of
the workspace, taken by its owners and administrators (`tilo workspace people
add`, `tilo workspace people invite`), not a setting of this image.

Pages reach the host over WebTransport (HTTP/3 on UDP), which no HTTPS ingress
carries, so the daemon's server configuration names the address the deployment
publishes. Native devices reach the host over iroh, through a packet relay: the
daemon runs its own, or is reached through one the deployment names.

## The deployment's environment

`entrypoint.sh` turns it into the server configuration and the daemon's
environment, and refuses any other `TILO_WORKSPACES_*` with exit status 2, so a
deployment that still sets what this image no longer reads fails at start instead
of running with a silent half of its settings.

Required:

| variable | what it is |
| --- | --- |
| `TILO_WORKSPACES_WEB_ENTRY` | the Tilo app's web entry the links the host mints open |

Optional, with these defaults:

| variable | default | what it is |
| --- | --- | --- |
| `TILO_WORKSPACES_STATE` | `/run/tilo-workspaces` | the volume the daemon's identity, profile, state and secrets live on |
| `TILO_WORKSPACES_LIVE` | none | the packet relay the daemon runs, written into a configuration it makes; `none` runs none |
| `TILO_KEPT` | none | the workspaces this host serves of its own, `ID=Name` comma-separated (or `ID` alone): its founder's device makes each on the first start and keeps it from then on |
| `TILO_WORKSPACES_ADMISSION` | `open` | who a kept workspace's entry lets in: `open` admits anyone who asks, at the default role; `invite` only people the workspace invited |
| `TILO_WORKSPACES_DESCRIPTION` | the workspace's name | what a kept workspace is, as the public directory shows it |
| `TILO_WORKSPACES_DEFAULT_ROLE` | `editor` | the role everyone who joins through the entry holds to begin with |
| `TILO_WORKSPACES_ROOT_FILE` | none | the owner's root, mounted from the operator's secrets: its 32-byte seed as 64 hexadecimal characters, readable by the image's user alone. A volume given it is that owner's own device and founds its kept workspaces; an empty volume then needs no ticket |
| `TILO_WORKSPACES_WAIT_LIST_MAX` | none | how many addresses an invitation-only kept workspace's wait list keeps |
| `TILO_WORKSPACES_CONTACT` | none | where someone asks to be taken off a kept workspace's wait list |
| `TILO_WORKSPACES_MAIL_FROM` | none | the sender a kept workspace's wait list mails an address from; mail needs `TILO_WORKSPACES_RESEND_KEY_FILE` |
| `TILO_WORKSPACES_MAIL_TEMPLATES` | none | the directory its wait-list mails' words are read from; the image's own words when none |
| `TILO_WORKSPACES_RESEND_KEY_FILE` | none | the file the mail API key is read from, kept among the deployment's secrets, never in a workspace's records |
| `TILO_WORKSPACES_RESEND_URL` | Resend's API | where mail is posted |
| `TILO_WORKSPACES_LOCAL` | none | this deployment's addresses are this machine's own (a local stack), so the links its mails carry open here |

A kept workspace's entry is settled once, when it is founded, and what its owners
change later stands: a later start reads the workspace's entry rather than
settling the deployment's again. Founding a workspace is its founder's own
device's act, so a volume is given its owner's root (`TILO_WORKSPACES_ROOT_FILE`):
the daemon keeps it as any of the owner's own devices keeps it, certifies its
device key under it and founds the workspaces its configuration keeps. A volume a
ticket joined keeps the workspaces its owners place on it (`servers add`) and
founds none.

`TILO_SERVER_CONFIG` names the server configuration the daemon reads (default
`$TILO_WORKSPACES_STATE/server.toml`), which the deployment writes when it fixes
the endpoint's address, a published packet relay or a host name.

`TILO_TICKET` is what an empty volume joins with instead: the ticket `tilo device
add` prints, left on the volume once and spent at the daemon's first start. A
volume that already holds its identity ignores it. A volume given its owner's root
never joins with a ticket: it is that owner's own device.

`TILO_SOURCE_REVISION` is the image's own revision; `TILO_SECRETS_DIR` already
points into the state volume, where the workspace secrets are kept sealed.

## The apps the workspace runs

A page runs the app it shows on the daemon, as any device does: the image
carries the four a device has — the launcher every app's process starts in
(`/usr/local/bin/tilo-launcher`, beside the daemon), the Tilo and tilolang
runtime bundles (`/usr/lib/tilo-runtime`), the key those and the packages are
signed with (`/etc/tilo/runtime-bundle-trusted-publishers.txt`), and the
default collection's tilolang packages, installed into the image's own store
(`/usr/share/tilo-apps`) by `install-apps.sh` through `tilo-install`. Nothing
about an app is special here: it is a signed package in the store, opened by
the name people know it by.

## State on the volume

```
$TILO_WORKSPACES_STATE/
  identity/       the device key, certificate and login the ticket left
  state/          the daemon's profile: workspaces, apps, actions
  secrets/        the secret store (TILO_SECRETS_DIR), sealed files
  credentials/    the ticket, while the daemon spends it
  server.toml     the server configuration, when TILO_SERVER_CONFIG names it here
```

## Building

`tools/ci/publish/lounge-binaries.sh` builds the programs,
`tools/ci/publish/lounge-apps.sh` the launcher, the runtimes and the signed
packages, `tools/ci/publish/lounge-fonts.sh` the product font catalog and
`tools/ci/publish/lounge-context.sh` lays out the build context. t's
`lounge-binaries`, `lounge-apps`, `lounge-fonts`, `image-lounge-context` and
`image-lounge` checks build and test this image; `image-web-entry-context`, `image-web-entry`
and `publish-web-entry` do the same for the web entry; `publish-lounge` and
`publish-web-entry` publish both from one commit, so a change is built by
naming the checks.
The checks that run the programs without an image around them (`e2e`)
ask `lounge-binaries.sh` for the profile Cargo.toml's
`lounge-check` names: the same code and optimisation level without the
whole-program link the image's own build spends.
`test-entrypoint.sh` asserts the environment-to-configuration mapping and
`test-image.sh` what the built image must carry.

## Using it before it is published

`test-e2e.sh` is the web page end to end, on every change that reaches it
(`t test e2e`): the tilo.sh stack runs through production's own entrypoints
for this image, the API server and the web entry (`os/containers/tilo-sh`'s
`stack.sh`), from what the checks beside it built, and every flow runs at the
same time against that one site, relay and workspace, each as people of its own in
headless Chromium: the page booting, a setup through an agent and its
resumes, the startup identity approving a server key and the
visa run beside it, fresh visitors' entry into the Lounge, the wait list of a
entry that takes people by invitation, a stranger's onboarding on a tilo.sh of
its own (`test-onboarding.sh`: the wait list, the two mails, the invitation the
person who runs the host sends from the workspace's wait list, and the
workspace of their own with their agent set up in it, the passkey that brings
them back on a browser that forgot the site, a second workspace, and a visitor
still refused). Each flow runs alone too:

```
bash os/containers/tilo-lounge/test-e2e.sh out/web-entry/bundle /tmp/e2e boot
bash os/containers/tilo-lounge/test-onboarding.sh out/web-entry/bundle /tmp/onboarding
```

It needs node, a headless Chrome (`CHROME`, else Playwright's cache), curl,
bwrap, `TILO_SH_PROGRAMS`, `TILO_SH_LOUNGE`, `TILO_SH_SITE` and
`TILO_SH_CADDY` naming what `tilo-sh-binaries`, `image-web-entry-context` and
`tilo-sh-caddy` built, and `TILO_SERVER_BIN`, `TILO_CRYPTO_BIN`,
`TILO_FONTS_DIR` and `TILO_CLI_BIN` naming built programs (the
`lounge-check-binaries`, `lounge-check-tools`, `lounge-fonts` and
`lounge-check-cli` outputs), which the wait list's own Lounge and the setups
run. Every wait in the page drivers is a
number of steps of `TILO_STEP_SECONDS` (5 s by default; `test-e2e.sh` sets
20 s for a node that draws every page in software), so a hang fails within its
few steps and names its step.
