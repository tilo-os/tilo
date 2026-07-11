# qOS App Registry

The registry is qOS's semi-federated marketplace for `.qpkg`
bundles. It advertises signed metadata so users can browse,
install, and update packages from the shell. Anyone can host
their own registry; the qOS org runs one at
`apps.qos.dev` (source under `crates/packaging/registry-server`)
and ships **the registry client itself as a `.qpkg` app** so it
can be swapped out or forked like any other qOS app.

This document covers the architecture, how to bootstrap a
self-hosted instance, publisher-allowlist workflow, TLS setup,
backup/restore, and where the moderation hooks live.

---

## Architecture

There are three layers:

1. **`registry-types`** (`crates/packaging/registry-types`)
   defines the wire types — `RegistryHandshake`,
   `RegistryEntry`, `EntryChangeRequest`. Serde JSON on the
   wire, postcard for the canonical signing form. Verification
   helpers live here so both the server and any client (native,
   web, CLI) share the same code.

2. **`registry-server`** (`crates/packaging/registry-server`) is
   a self-hostable HTTP binary that stores entries on disk and
   verifies every mutating request against an owner-controlled
   publisher allowlist.

3. **A registry client** (packaged as a `.qpkg` and installed on
   qOS) queries the server, prompts the user on first pin, and
   drives `pkg-cli install` under the hood. Not part of this
   commit — the server + types come first.

### Trust chain

```
                   +------------------+
                   |  Registry Owner  |     <- pins clients: "trust me"
                   |  Ed25519 pubkey  |
                   +---------+--------+
                             | signs
                             v
        +----------------+  publishers.toml   +---------------------+
        | Allowlisted    | ---(is member)---> | RegistryEntry: sig, |
        | Publisher key  |                    | qpkg_url, root hash |
        +----------------+                    +----------+----------+
                                                         | client downloads
                                                         v
                                                 +--------------+
                                                 |    .qpkg     |
                                                 |  pkg::verify |
                                                 +--------------+
```

The **owner** signs an index snapshot for federation
(`INDEX_DOMAIN`). Each **publisher** signs their own entry
(`ENTRY_DOMAIN`). The owner-controlled `publishers.toml`
allowlists which publisher pubkeys the server accepts POSTs from
and whether each publisher may add new entries or only edit /
delete their own existing ones.

Trust flow on the client:
- Client pins the **owner pubkey** (from `RegistryHandshake`).
- Client verifies each **entry signature** against the entry's
  declared `publisher_pubkey_hex`, then checks that pubkey
  against the allowlist snapshot the owner signed.
- Client downloads the `.qpkg`, verifies the BLAKE3 root hash
  matches the `qpkg_root_hash_hex` in the entry, then runs
  `pkg::verify` (which itself checks the publisher's own
  signature over the manifest — a second, independent proof).

### Canonical signing form

```
publisher entry sig  = ed25519_sign(publisher_key,
                                    blake3(b"qos-registry-entry-v1\0"
                                           || postcard(entry_without_signature)))

publisher change sig = ed25519_sign(publisher_key,
                                    blake3(b"qos-registry-entry-v1\0"
                                           || postcard((action,
                                                        entry_without_signature))))

owner index sig      = ed25519_sign(owner_key,
                                    blake3(b"qos-registry-index-v1\0"
                                           || postcard(index_manifest)))
```

Postcard was chosen over JSON because JSON's key order and
whitespace are not deterministic across encoders — signatures
generated on one host would refuse to verify after a re-encode
on the other. Postcard fixes the byte order per struct field
and the length prefix on collections. See
`crates/packaging/pkg::trust` for the same pattern applied to
`.qpkg` attestation signatures.

## REST API

```
GET  /registry            → RegistryHandshake         (server identity)
GET  /health              → { "ok": true }
GET  /entries             → [RegistryEntry, …]
GET  /entries/<pkg-id>    → RegistryEntry              (404 if not present)
POST /entries             → EntryChangeRequest body    (200/201/404/409/403)
```

`POST /entries` accepts an `EntryChangeRequest` with an `action`
of `add`, `update`, or `delete`. All three must carry:

- a valid **change-level** publisher signature over the (action,
  entry-without-sig) preimage;
- a valid **entry-level** publisher signature over the
  entry-without-sig preimage;
- a `publisher_pubkey_hex` matching a row in `publishers.toml`.

`update` and `delete` additionally require that the
`publisher_pubkey_hex` matches the stored entry's — publishers
may only edit their own entries. `update` further requires
`updated_at_unix > stored.updated_at_unix` to reject stale
replays.

## Bootstrapping your own instance

### 1. Generate an owner keypair

Use `pkg-cli genkey` (or any Ed25519 CLI). Keep the private key
in a hardware token if you can; publish only the pubkey.

### 2. Init the data dir

```
qos-registry-server init \
    --owner-pubkey <hex> \
    --name "acme registry" \
    --description "Apps for the acme fleet" \
    --base-url https://apps.acme.example
```

This writes `handshake.toml` under `$XDG_STATE_HOME/qos-registry`
(default: `~/.local/state/qos-registry`, or `/var/lib/qos-registry`
under the systemd unit).

### 3. Allowlist your first publisher

```
qos-registry-server add-publisher \
    --pubkey <publisher-hex> \
    --can-add-new \
    --note "in-house team"
```

`--can-add-new` says this publisher may POST new entries. Without
it, they may only edit or delete their own existing rows —
useful for onboarding a publisher who's inheriting an existing
entry.

### 4. Serve

```
qos-registry-server serve --port 8443 --bind 127.0.0.1
```

Behind nginx / Caddy — see TLS below.

### 5. Publisher workflow

Publishers build a `RegistryEntry`, sign it with `sign_entry`
(from `registry-types`), then POST an `EntryChangeRequest` to
`/entries`. The convenience CLI (`pkg-cli publish-to-registry`,
not landed yet) automates this against a locally-built
`.qpkg`.

## TLS

TLS is deliberately **out of scope** for the server binary. All
security guarantees on the wire come from the Ed25519 signatures
covering entries and change requests — clients verify these
against the pinned owner pubkey regardless of whether the
transport is HTTPS.

That said, for deployments you almost always want a real
TLS-terminating reverse proxy in front:

- Rotate certificates without restarting the registry.
- Rate-limit and cache GETs at the edge.
- Serve the `.qpkg` payloads themselves — the entries just point
  at HTTPS URLs; the actual bytes can live on any CDN / object
  store.

### nginx example

```
server {
    listen 443 ssl http2;
    server_name apps.acme.example;
    ssl_certificate     /etc/letsencrypt/live/apps.acme.example/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/apps.acme.example/privkey.pem;

    location / {
        proxy_pass http://127.0.0.1:8443;
        proxy_set_header Host $host;
    }

    location /packages/ {
        alias /srv/qpkg/;         # your .qpkg bytes served straight
        autoindex off;
    }
}
```

### Caddy example

```
apps.acme.example {
    reverse_proxy /registry* /entries* /health 127.0.0.1:8443
    file_server /packages/* {
        root /srv/qpkg
    }
}
```

## Backup and restore

Everything the server owns is under the data directory:

```
data-dir/
    handshake.toml       # server identity — owner pubkey + name / description / base_url
    publishers.toml      # allowlist
    entries/
        <pkg-id>.json    # one entry per file
```

All writes are atomic-rename (`.tmp` → real path), so
`tar -czf backup.tgz data-dir/` while the server is running is
safe — the worst case is that you capture a subset of
in-flight changes; nothing gets torn.

For a restore, drop the same tarball back in place and start
`qos-registry-server serve`. There's no daemon-side state
outside the data-dir — the server is stateless across restarts.

## Moderation hooks

Registry owners moderate by editing the allowlist and the on-disk
entry files:

- **Ban a publisher** — remove them from `publishers.toml` with
  `qos-registry-server remove-publisher --pubkey <hex>`. Their
  historic entries stay in place until you delete them (below).
- **Force-delete an entry** — remove the JSON file from
  `entries/` directly. Clients cache handshakes and re-poll
  entries; nothing else has to happen server-side.
- **Content policy** — the server treats entries as opaque
  metadata. Category / age-rating filtering is expected to
  live in the client UI, driven by categories on
  `RegistryEntry`.
- **Federation snapshots** — a moderator can serve a signed
  index snapshot (using the `INDEX_DOMAIN` signing form
  exposed by `registry-types`) so downstream mirrors can pin a
  particular curation. The index-manifest schema is
  intentionally left open here — projects that want to build
  a mirror pipeline can define one on top of this crate's
  primitives.

## Threat model — what this design does NOT protect against

- **Compromised registry owner** — the owner can inject any
  publisher into the allowlist and thus indirectly cause any
  entry to be accepted. Clients that pin an owner and lose
  trust must un-pin and re-pin a new owner.
- **Compromised publisher key** — an attacker with a publisher's
  private key can update the entry to point at malicious
  bytes. The client verifies BLAKE3 root hashes on the `.qpkg`,
  which stops in-flight tampering but not a compromised
  publisher signing new hashes. Publishers should keep signing
  keys in hardware and rotate on compromise; the qOS threshold
  identity work (`crates/collab/threshold-identity`) is the
  long-term path to publisher-key resilience.
- **Denial of service** — no rate limiting is built in. Put the
  reverse proxy in front, or run `fail2ban` against
  `journalctl -u qos-registry-server`. All POSTs perform an
  Ed25519 verification before touching disk, so unsigned junk
  is cheap to reject.
