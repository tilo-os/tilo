# A Tilo machine as a node

A node is a machine that runs a person's rooms for their web pages: a relay
that serves browsers (`tilo-relay run --web`) and, beside it, a headless Tilo
session that runs their apps and streams the windows to the pages as paint
streams. The person's root stays in their page; the machine never holds it.

## Provisioning (the operator)

1. Choose the account the session runs as (`quickuser` on a dev VM) and write
   a random install token to `/etc/tilo/node/install-token` (mode `0600`,
   owned by `tilo-relay`).
2. Enable the node:

   ```sh
   systemctl enable --now tilo-node-relay@ACCOUNT.service tilo-node-session@ACCOUNT.path
   ```

   - `tilo-node-session-key@ACCOUNT` creates the session's device key as that
     account under `/var/lib/tilo-node/ACCOUNT/session` and writes its public
     half to `session.env` (`tilo-device-key-init --root … --public-env-out …`).
   - `tilo-node-relay@ACCOUNT` (the `tilo-relay` account, sandboxed as
     `tilo-relay.service`, which it conflicts with) creates the relay's own
     device key under `/var/lib/tilo-node/relay`, prints
     `{"event": "awaiting_certificate", "devicePublicKey": …}` and serves only
     `POST /peer/v1/install` on port 9443 until the person installs it.
3. Name where the pages are served from in `/etc/tilo/node/web.env`:
   `TILO_NODE_WEB_ARGS="--allowed-origin https://PAGES --web-entry https://PAGES/"`.
4. Hand the person the machine's origin (`https://HOST:9443`), its device key
   and the token.

## Installing (the person)

The person's page installs the node with its root (`installNode` in
`web/root-identity.mjs`). The relay then:

- writes its own host certificate and starts as the person's host on the same
  origin;
- writes the session's certificate and the person's user key into the
  session's identity tree (`--session-identity-root`; public halves only,
  never the session's private key);
- writes its endpoint as `TILO_COLLAB_HOST` to `/run/tilo-node/ACCOUNT/session-host.env`
  (`--session-env-out`).

The certificate's arrival starts `tilo-node-session@ACCOUNT` (a path unit): a
headless session with `TILO_IDENTITY_ROOT` on that tree, which joins as one of
the person's devices. It never starts before, so it never mints a root of its
own. On start it completes its side of the join, as any joined device holds
its person: a public record naming them as its active identity, and a fleet
that knows it. Those live under `XDG_CONFIG_HOME=/var/lib/tilo-node/ACCOUNT/config`,
apart from any desktop session the account has.

## Using it

The page creates rooms on the node and invites people to them. Opening an app
from a page invokes `app.<id>/open` on the session (`/peer/v1/node`), which
launches it in the room. A headless session streams every legacy window of a
workspace whose room has a host to that host's viewers, as paint streams to
pages that draw them (`crates/hosting/tilo-relay/LEGACY_WINDOWS.md`).
