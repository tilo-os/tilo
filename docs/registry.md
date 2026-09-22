# Tilo App Registry

The registry is Tilo's semi-federated marketplace for `.tapp`
bundles. It advertises signed metadata so users can browse,
install, and update packages from the shell. Anyone can host
their own registry. The Tilo deployment is publicly reachable at `apps.tilo.so`
using the server under `crates/packaging/registry-server`.

Continuous app packaging currently publishes immutable tapps and collection
bundles to Forgejo's generic package registry. That is the cross-workflow
artifact channel used by app, desktop and OS jobs; it is distinct from the
signed catalogue and moderation service described here. Main-branch app publication
now submits newly built tapps to that service; separate collector and networkless
signer jobs independently verify accepted requests before advancing its signed
snapshot. Production readiness still depends on the deployed owner/publisher trust,
delegated-key rotation and backup path.

The signed client logic ships in the host `app` command and powers the graphical
Store's repository view.

The registry architecture covers self-hosted bootstrap, publisher-allowlist
workflow, TLS setup, backup/restore, and moderation hooks.

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
   publisher allowlist, stages it for review, and exposes only
   approved entries to clients.

3. **The shared catalogue client** queries the server, requires a
   preconfigured owner pin, verifies the complete snapshot, and drives the
   verified installer path. The Store and desktop search use this same client.

### Trust chain

```
 Registry owner offline key
            | signs a short-lived, publisher-scoped channel authorization
            v
 Delegated channel key (networkless) <--- independently verifies staged tapp bytes
            | signs exact policy + entries + expiry + moderation head
            v
 SignedRegistrySnapshot <--- publisher signs each RegistryEntry
            |
            | pinned owner verification + durable anti-rollback checkpoint
            v
      verified .tapp root ---> tilo_app_package::verify publisher identity again
```

The **owner** signs an index snapshot for federation
(`INDEX_DOMAIN`). Each **publisher** signs their own entry
(`ENTRY_DOMAIN`). The owner-controlled `publishers.toml`
allowlists which publisher pubkeys may submit changes and whether
each publisher may propose new entries or only changes to entries
they already own. Admission is deliberately not approval: every
accepted add, update, and delete remains private until an owner
reviews it.

Trust flow on the client:
- Client pins the **owner pubkey** (from `RegistryHandshake`).
- Client verifies each **entry signature** against the entry's
  declared `publisher_pubkey_hex`, then checks that pubkey
  against the allowlist snapshot the owner signed.
- Client downloads the `.tapp`, verifies the BLAKE3 root hash
  matches the `tapp_root_hash_hex` in the entry, then runs
  `tilo_app_package::verify` (which itself checks the publisher's own
  signature over the manifest — a second, independent proof).

### Canonical signing form

```
publisher entry sig  = ed25519_sign(publisher_key,
                                    blake3(b"tilo-registry-entry-v1\0"
                                           || postcard(entry_without_signature)))

publisher change sig = ed25519_sign(publisher_key,
                                    blake3(b"tilo-registry-entry-v1\0"
                                           || postcard((action,
                                                        entry_without_signature))))

owner index sig      = ed25519_sign(owner_key,
                                    blake3(b"tilo-registry-index-v1\0"
                                           || postcard(index_manifest)))

delegated index sig  = ed25519_sign(channel_key,
                                    blake3(b"tilo-registry-index-v3\0"
                                           || postcard(index_manifest
                                                       + root authorization)))
```

Postcard was chosen over JSON because JSON's key order and
whitespace are not deterministic across encoders — signatures
generated on one host would refuse to verify after a re-encode
on the other. Postcard fixes the byte order per struct field
and the length prefix on collections. See
`crates/packaging/tilo-app-package::trust` for the same pattern applied to
`.tapp` attestation signatures.

## REST API

```
GET  /registry            → RegistryHandshake         (server identity)
GET  /health              → { "ok": true }
GET  /entries             → SignedRegistrySnapshot
GET  /entries/<pkg-id>    → SignedRegistrySnapshot      (404 if not present)
POST /entries             → EntryChangeRequest body    (202/404/409/403)
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
replays. A valid request returns `202 Accepted` with
`{"status":"pending_review","id":"<package-id>"}`. GET requests
never expose pending payloads. They expose only the most recently published,
owner-signed snapshot: approval changes staging state, and the public catalogue
changes only after the offline signing step below.

## Bootstrapping your own instance

### 1. Generate an owner keypair

Use `tilo-app-package-cli genkey` (or any Ed25519 CLI). Keep the private key
in a hardware token if you can; publish only the pubkey.

### 2. Init the data dir

```
tilo-registry-server init \
    --owner-pubkey <hex> \
    --name "acme registry" \
    --description "Apps for the acme fleet" \
    --base-url https://apps.acme.example
```

This writes `handshake.toml` under `$XDG_STATE_HOME/tilo-registry`
(default: `~/.local/state/tilo-registry`, or `/var/lib/tilo-registry`
under the systemd unit).

### 3. Allowlist your first publisher

```
tilo-registry-server add-publisher \
    --pubkey <publisher-hex> \
    --can-add-new \
    --note "in-house team"
```

`--can-add-new` says this publisher may submit new entries. Without
it, they may only propose edits or deletion of their own rows —
useful for onboarding a publisher who's inheriting an existing
entry.

### 4. Serve

```
tilo-registry-server serve --port 8443 --bind 127.0.0.1
```

The production OCI image uses the same interface and contains no TLS
termination or private signing material:

```text
code.podesta.ai/qos/tilo-registry-server@sha256:<digest>
/usr/local/bin/tilo-registry-server \
  --data-dir /var/lib/tilo-registry serve --bind 0.0.0.0 --port 8443
```

It runs as UID/GID 65532 and supports a read-only root filesystem. Mount the
state directory read-write, mount `/tmp` as a small tmpfs, and probe
`GET /health` on port 8443. Main-branch CI publishes only an immutable
`sha-<full-source-revision>` tag and records the deployable digest in the
`registry-server-image-receipt` workflow artifact.

Behind nginx / Caddy — see TLS below.

### 5. Publisher workflow

Publishers build a `RegistryEntry`, sign it with `sign_entry`
(from `registry-types`), then POST an `EntryChangeRequest` to
`/entries`. Publisher tooling should derive the advertised root hash and
size from the final locally-built `.tapp`, never from an intermediate build.
CI can generate the complete request from those final bytes with the same
identity that signed the package:

```text
tilo-app-package entry \
  --identity /run/secrets/app-publisher.json \
  --action update \
  --previous-entry current-entry.json \
  --metadata registry-metadata.json \
  --url https://apps.tilo.so/artifacts/immutable/app.tapp \
  app.tapp > request.json
curl --fail --header 'Content-Type: application/json' \
  --data-binary @request.json https://apps.tilo.so/entries
```

Use `--action add` for the first version. The command refuses an identity that
does not match the publisher embedded in the tapp, so the registry submission
cannot relabel artifacts signed by another key. Updates require the current
signed entry and append to its immutable version history; re-running the same
artifact reproduces the existing entry, while reusing a version/target identity
for different bytes fails closed.

Presentation metadata is a signed part of the entry. Localized fields use one
consistent language-tag map, and every non-empty localized map must contain an
English (`en`) fallback. Language tags are canonical BCP 47 spellings (for
example `en`, `de`, or `zh-Hant-TW`); empty values, malformed tags such as
`en_US`, and unsorted or duplicate supported-language lists are rejected.
`supported_languages` describes languages the app itself supports, not merely
the locales supplied for its store listing. Example authoring input:

```json
{
  "name": {"en": "Example", "de": "Beispiel"},
  "description": {
    "en": "A focused example app for Tilo OS.",
    "de": "Eine fokussierte Beispiel-App für Tilo OS."
  },
  "tagline": {"en": "Learn by example", "de": "Am Beispiel lernen"},
  "images": {
    "en": [
      {
        "kind": "icon",
        "url": "https://apps.tilo.so/images/example-icon.png",
        "alt": "Example app icon"
      },
      {
        "kind": "screenshot",
        "url": "https://apps.tilo.so/images/example-main.png",
        "alt": "Example app main screen"
      }
    ]
  },
  "supported_languages": ["de", "en"],
  "source_url": "https://code.podesta.ai/example/example",
  "homepage": "https://example.invalid"
}
```

Clients resolve exact locale, then its base language, then English. Images use
the same fallback rule and distinguish `icon`, `screenshot`, and `banner`.

Each signed version binds the package root, exact final-file BLAKE3 and size,
platform family, architecture, minimum ISA and runtime ABI. The schema can add
future platform families without changing the signed entry shape, but the
official registry currently accepts only `platform = "tilo-os"` with
`architecture = "x86_64"` or `"aarch64"`. A release must publish a distinct
native `.tapp` for both architectures. Pass each tapp's target with
`tilo-app-package entry --arch`; generic Linux, macOS, and Windows builds do not
belong in the Tilo OS app channel.

### 6. Moderator workflow

Publisher requests are stored in `pending/`, one retry-safe slot per
package. The owner reviews them locally; there is intentionally no
remotely callable approval endpoint.

```text
tilo-registry-server review list --json
tilo-registry-server review show so.tilo.example --json
tilo-registry-server review approve so.tilo.example \
    --reviewer alice --reason "capabilities and core journeys verified"
tilo-registry-server review reject so.tilo.example \
    --reviewer alice --reason "misleading subscription disclosure"
```

`show` and `approve` fetch every declared package version with strict
size and time bounds, refuse redirects and private-network destinations,
re-derive the content-addressed identity and publisher signature, and
compare the signed manifest's app id and version with the submission.
Their output includes filesystem, network, portal, and service requests.
Approval moves the exact signed metadata into snapshot staging; a moderator
cannot quietly rewrite a publisher's submission.

Publish the next bounded-lifetime snapshot from an offline signing environment:

```text
chmod 0600 /secure/registry-owner.seed
tilo-registry-server sign-snapshot \
  --owner-key-file /secure/registry-owner.seed \
  --valid-for-seconds 86400
```

Production does not put the root key in a recurring job. During bootstrap (or
manual rotation), the offline root authorizes one short-lived channel key and
binds it to the exact publisher policy, maximum snapshot lifetime, and entry
count. Installing a higher-generation authorization immediately revokes the
old online key at the registry; certificates expire after at most 31 days.

```text
tilo-registry-server authorize-channel \
  --owner-key-file /secure/registry-owner.seed \
  --channel-pubkey "$CHANNEL_PUBKEY" \
  --publisher-pubkey "$FIRST_PARTY_PUBLISHER" \
  --generation 1
```

Each recurring promotion has two security boundaries. A networked collector
has no signing key and writes exact tapp bytes under their BLAKE3 names. A
separate networkless signer receives only the delegated key; it re-verifies the
publisher request, live allowlist, staged-byte digest, tapp root/signature,
manifest identity, target policy, delegation scope, predecessor, sequence and
expiry before approving or signing anything:

```text
tilo-registry-server collect-first-party \
  --publisher-pubkey "$FIRST_PARTY_PUBLISHER" --max-items 50

tilo-registry-server publish-staged-first-party \
  --channel-key-file /run/secrets/registry-channel/seed \
  --reviewer registry-promoter \
  --reason automated-first-party-promotion \
  --valid-for-seconds 86400

# A separate networkless schedule refreshes expiry only when no batch is staged.
tilo-registry-server sign-delegated-snapshot \
  --channel-key-file /run/secrets/registry-channel/seed \
  --valid-for-seconds 86400
```

Both seed files are exactly 32 secret bytes encoded as 64 hex characters. The
root seed exists only in the bootstrap/manual authorization environment. The
serving process receives neither private key. The collector cannot forge a
stage because the signer treats its JSON as untrusted and independently checks
the content-addressed bytes. The signer has no network path from which to fetch
or substitute content.

Every successful signing also retains the exact signed JSON under
`snapshots/<sequence>-<hash>.json` before advancing `published-snapshot.json`.
These small immutable files are the channel history and should share the data
directory's backup policy. Roll back catalogue contents by publishing a new,
higher sequence from one of them:

```text
tilo-registry-server rollback-snapshot \
  --to-sequence 41 \
  --owner-key-file /secure/registry-owner.seed \
  --valid-for-seconds 86400
```

The command does not replay sequence 41. It signs sequence `current + 1`, links
it to the current snapshot, uses the current publisher allowlist, and refuses
to restore entries from a publisher that has since been removed. Client
anti-rollback checkpoints therefore remain monotonic.

For urgent removal, `review quarantine` immediately moves a public entry
out of search while keeping its bytes and decision record recoverable.
`review restore` re-checks the signature and current allowlist before
making it visible again.

## TLS

TLS is deliberately **out of scope** for the server binary. All
security guarantees on the wire come from the Ed25519 signatures
covering entries and change requests — clients verify these
against the pinned owner pubkey regardless of whether the
transport is HTTPS.

For deployments, use a TLS-terminating reverse proxy in front:

- Rotate certificates without restarting the registry.
- Rate-limit and cache GETs at the edge.
- Serve the `.tapp` payloads themselves — the entries just point
  at HTTPS URLs; the bytes can live on any CDN / object
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
        alias /srv/tapp/;         # your .tapp bytes served straight
        autoindex off;
    }
}
```

### Caddy example

```
apps.acme.example {
    reverse_proxy /registry* /entries* /health 127.0.0.1:8443
    file_server /packages/* {
        root /srv/tapp
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
        <pkg-id>.json    # approved public entries only
    pending/
        <pkg-id>.json    # signed requests awaiting review
    reviews/
        <review-id>.json # immutable moderator audit records
    quarantined/
        <pkg-id>.json    # recoverable entries hidden from clients
    snapshots/
        <sequence>-<hash>.json # immutable signed channel history
    published-snapshot.json    # atomic current channel pointer
```

All writes are atomic-rename (`.tmp` → real path), so
`tar -czf backup.tgz data-dir/` while the server is running is
safe — the worst case is that you capture a subset of
in-flight changes; nothing gets torn.

For a restore, drop the same tarball back in place and start
`tilo-registry-server serve`. There's no daemon-side state
outside the data-dir — the server is stateless across restarts.

## Moderation policy and operations

- **Ban a publisher** with `remove-publisher`. Pending work from that
  key can no longer be approved, and restoring quarantined entries checks
  the current allowlist.
- **Quarantine an entry** through the review CLI, never by deleting files
  by hand. This removes it from `/entries`, keeps the exact publisher-signed
  entry recoverable, and records who acted and why.
- **Review the package, not its name or screenshot.** Approval verifies
  immutable package bytes and surfaces sandbox capabilities. Operators
  should also publish and apply rules for privacy, billing disclosure,
  impersonation, illegal content, and abusive behavior.
- **Keep decisions explainable.** Reviewer identity, reason, time, exact
  submission, and decision are durable JSON records under `reviews/`.
- **Federation snapshots** use the versioned `SignedRegistrySnapshot` schema.
  Clients verify the pinned owner signature, validity window, monotonic durable
  checkpoint, every publisher signature, and membership in the exact signed
  publisher policy. Bare entry arrays and TOFU are rejected.

## Threat model — what this design does NOT protect against

- **Compromised registry owner** — the owner can inject any
  publisher into the allowlist and thus indirectly cause any
  entry to be accepted. Clients that pin an owner and lose
  trust must un-pin and re-pin a new owner.
- **Compromised publisher key** — an attacker with a publisher's
  private key can submit malicious bytes, but cannot replace the
  public entry without owner review. Package and registry signatures
  stop in-flight substitution; human review is still fallible, so
  operators should quarantine suspicious entries and revoke the key.
  Publishers should keep signing keys in hardware and rotate on
  compromise; the Tilo threshold
  identity work (`crates/collab/threshold-identity`) is the
  long-term path to publisher-key resilience.
- **Denial of service** — no rate limiting is built in. Put the
  reverse proxy in front, or run `fail2ban` against
  `journalctl -u tilo-registry-server`. All POSTs perform an
  Ed25519 verification before touching disk, so unsigned junk
  is cheap to reject.
