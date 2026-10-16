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

bot entry/change sig = the same, under b"tilo-registry-bot-entry-v1\0"

bots sig             = ed25519_sign(owner_or_channel_key,
                                    blake3(b"tilo-registry-bots-v1\0"
                                           || postcard((index digest, bots))))

owner index sig      = ed25519_sign(owner_key,
                                    blake3(b"tilo-registry-index-v1\0"
                                           || postcard(index_manifest)))

delegated index sig  = ed25519_sign(channel_key,
                                    blake3(b"tilo-registry-index-v3\0"
                                           || postcard(index_manifest
                                                       + root authorization)))
```

Bot entries were added without changing what any released client reads.
An app entry encodes, signs and verifies byte for byte as before: its
`kind` is omitted and its versions keep their flat `tapp_*` fields. A bot
entry carries `"kind": "bot"`, its versions carry `bot` instead of the
`tapp_*` fields, and its signature uses the bot domain, so neither kind can
pass for the other. Snapshots list bot entries under `bots`, beside
`entries`, with `bots_signature_hex`: the same key's signature over the bot
entries and the digest the snapshot's own signature covers (the compact
form uses `tilo-registry-compact-bots-v1`). The owner signature and the
snapshot hash cover `entries` only, so a client that predates bots ignores
both new fields and verifies the snapshot unchanged. See
[architecture/bots.md](architecture/bots.md) §10 for bot entries.

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
GET  /entries?format=compact → CompactRegistrySnapshot  (the full one until it exists)
GET  /entries/<pkg-id>    → SignedRegistrySnapshot      (404 if not present)
GET  /indexes/<blake3>.json → RegistryVersionIndex      (named by a compact snapshot)
POST /entries             → EntryChangeRequest body    (202/404/409/403)
GET  /thin/<root>.tapp    → thin form of a reviewed package, by package root
GET  /blobs/<blake3>      → one payload blob of a reviewed package
GET  /artifacts/<blake3>.tapp → a full .tapp stored with `import-artifact`
GET  /blobs/<blake3>.zst, /artifacts/<blake3>.tapp.zst → their compressed twins
PUT  /uploads/<blake3>.tapp    → a publisher's package, private until review
PUT  /uploads/<blake3>.program → a publisher's bot program, private until review
                                 (201/400/401/403/413/422)
```

`/entries` carries `Cache-Control: no-cache` and `ETag: "<snapshot hash>"`
and answers a matching `If-None-Match` with `304`, so a returning client
costs one small revalidation; the compact form has its own ETag. The
content-addressed routes are
immutable (`Cache-Control: public, max-age=31536000, immutable`); the server
re-checks every file against its name before serving it, and clients verify
again. A blob that is a WebAssembly binary is served as `application/wasm`
so browsers can compile it while it streams; everything else is
`application/octet-stream`, with `X-Content-Type-Options: nosniff`.

Approving a submission (`review approve`) stores the thin form and payload
blobs of every reviewed version, so a client that already has some blobs of
an earlier release downloads only the new ones.

### Compressed twins

Packages and payload objects travel compressed. Beside each content-addressed
object the registry keeps a **twin**, the same bytes as one zstd frame at
level 19 (`tilo_app_package::compressed`): `/blobs/<blake3>.zst` for every
blob and chunk, `/artifacts/<blake3>.tapp.zst` for every imported package.
The release pipeline uploads `<file>.tapp.zst` beside every tapp it
publishes (`tilo-app-package compress`). Measured on real packages, twins are
34–42 % of an app tapp and 32 % of the runtime's payload.

A twin is transport only. Signatures, `tapp_blake3_hex`, `tapp_size_bytes`,
blob names and chunk lists all describe the uncompressed bytes, and nothing
signs the twin. A client asks for `<url>.zst` first, decompresses it while it
streams with a hard bound — the signed `tapp_size_bytes` for a package, the
listed length for a chunk, the 64 MiB object ceiling for a whole blob — so a
small twin cannot expand into more than the object it stands for, and then
verifies the decompressed bytes exactly as it verifies a raw download. A host
without a twin answers 404 and the client fetches the raw object, so packages
published before twins and registries that keep none keep working; clients
that predate twins never ask for them and get raw bytes as before. A twin is
labelled `application/zstd` and never sent with `Content-Encoding`, so no
proxy or HTTP client decodes it on the way.

The registry writes a twin when it stores the object, from bytes it has
already checked, and re-checks the twin against the object's hash before
serving it. Pruning removes a twin with its object. For objects stored before
twins existed, run once after upgrading:

```
tilo-registry-server compress-payloads
```

It prints how many twins it wrote and their size, and is safe to rerun while
`serve` runs.

**Browsers.** `serve --cors-origin <origin>` (repeatable) lets pages on an
exact serialized origin such as `https://tilo.so` or `http://localhost:4321`
read GET responses: those responses carry `Vary: Origin` and, for a listed
`Origin`, `Access-Control-Allow-Origin: <origin>`. Nothing else is granted:
no credentials, no preflight answers, no cross-origin `POST`. Without the
flag no CORS headers are sent. Clients revalidate the snapshot with
`fetch(url, {cache: 'no-cache'})`, which lets the browser add
`If-None-Match` itself; setting the header by hand would need a preflight.

`POST /entries` accepts an `EntryChangeRequest` with an `action`
of `add`, `update`, or `delete`. All three must carry:

- a valid **change-level** publisher signature over the (action,
  entry-without-sig) preimage;
- a valid **entry-level** publisher signature over the
  entry-without-sig preimage;
- a `publisher_pubkey_hex` matching a row in `publishers.toml`.

`add` and `update` also require the `package_id` to lie in one of that row's
`namespaces`: `com.acme.notes` needs `com.acme`. Package ids are
lowercase reverse DNS with at least three labels
(`registry_types::namespace`). A row without `namespaces` (or with an empty
list) owns nothing: it can still delete its own entries but can add or update
none. `so.tilo` is granted only by naming it.

`update` and `delete` additionally require that the
`publisher_pubkey_hex` matches the stored entry's — publishers
may only edit their own entries. `update` further requires
`updated_at_unix > stored.updated_at_unix` to reject stale
replays. A valid request returns `202 Accepted` with
`{"status":"pending_review","id":"<package-id>"}`. GET requests
never expose pending payloads. They expose only the most recently published,
owner-signed snapshot: approval changes staging state, and the public catalogue
changes only after the offline signing step below.

### Publisher applications

A key that is not allowlisted yet asks for namespaces with
`POST /publishers/applications`, sending a signed
`registry_types::application::PublisherApplication`: the key, up to eight
claims, a contact address for the owner and a note. Each claim is proven by
publishing the line `tilo-publisher=<pubkey hex>` where only the namespace's
owner can write it:

| Claim | Namespace | Where the proof lives |
| --- | --- | --- |
| domain `example.com` | `com.example` | DNS TXT at `_tilo-publisher.example.com`, else `https://example.com/.well-known/tilo-publisher.txt` |
| GitHub account `ada` | `io.github.ada` | `tilo-publisher.txt` in the public repository `ada/tilo-publisher` |
| Codeberg account `ada` | `org.codeberg.ada` | the same file and repository on Codeberg |
| Forgejo or Gitea at `git.example.net`, account `ada` | `net.example.git.ada` | the same file and repository on that host |

The registry verifies the signature, refuses applications dated more than a
day from its clock, reads every proof and answers `202 Accepted` with the
pending status, or `422` naming the namespace, the expected line and every
place it looked. The application then waits for the owner;
`GET /publishers/applications/<pubkey>` reports its status and proven
namespaces but never the contact address. Nothing is approved over HTTP:

```
tilo-registry-server applications list
tilo-registry-server applications show <pubkey> --json
tilo-registry-server applications approve <pubkey> --reviewer <name>
tilo-registry-server applications reject <pubkey> --reviewer <name> --reason <text>
```

`approve` reads the proofs again, so a proof withdrawn while the application
waited fails the approval and changes nothing. Otherwise it allowlists the key
with `--can-add-new` for the proven namespaces, adding them to any it already
had.

For a UI or a script that decides what it previously displayed, take `review_id`
from `applications show --json` and pass it to either decision:

```
tilo-registry-server applications approve <pubkey> --reviewer <name> --review-id <id>
tilo-registry-server applications reject <pubkey> --reviewer <name> --reason <text> --review-id <id>
```

The id identifies the complete stored application, including its signed claims
and review state. If the publisher replaces it, refresh and review the new
request; an old decision changes neither the allowlist nor the replacement.
Approval also compares the reviewed snapshot under the transaction lock after
checking proofs, so a replacement arriving during a DNS/HTTPS check cannot be
overwritten. Rust callers use `applications::approve_reviewed` or
`applications::reject_reviewed` with the displayed `StoredApplication`.
The original CLI forms remain supported. These commands use existing local
operator access; they introduce no remotely callable approval endpoint.

## Native operator review

On the registry host, the native operator tool uses the same local review
library as the CLI. Build it with `cargo build --locked -p registry-review`,
then open an explicitly private window:

```bash
target/debug/tilo-registry-review --private --registry-dir /path/to/registry
```

Choose **Refresh applications**, inspect the signed request, its exact
namespaces, contact, note and proof locations, then name the reviewer.
**Approve namespaces** checks ownership proofs again. **Reject with feedback**
returns the written reason through the publisher's existing status endpoint.
Both decisions require confirmation and refuse a replacement application;
refresh and inspect the new request before deciding again.

The pending queue is device-local view state. Only reviewer/feedback drafts
use the SDK document and its undo/history on a disposable private profile;
no registry queue or contact data is synced. Revoking owner access hides the
review pane and refuses its actions. A shared launch reads no registry data.
The tool needs the operator's existing filesystem access to an initialized
registry; it grants no access, opens no approval service and holds no registry
signing key. Public publication still uses the registry's existing signing
workflow. The same window also reviews package submissions through the CLI's shared signed-byte inspection and approval workflow.

The action surface is `Refresh`, `Inspect {publisher}`, followed by
`Approve {publisher, review_id, reviewer}` or
`Reject {publisher, review_id, reviewer, reason}`. `Inspect` returns the exact
snapshot and review id; automated operators use the same confirmation and
stale-review checks as the controls. Queue pages contain at most 20 rows.

Choose **Refresh packages** to list pending package summaries without downloading
anything. **Verify and inspect packages** explicitly reads uploaded artifacts or
fetches external versions with the CLI's bounded, public-network-only policy.
The detail view shows verified versions, byte counts and signed sandbox requests;
review never launches a package. Verification errors remain visible and block
approval while leaving rejection available. Name the reviewer and write a reason
before either confirmed decision. Approval rechecks bytes, current authorization
and the exact pending request before staging metadata and publishing reviewed
uploads. Signed registry snapshot publication remains a separate operator step.

Package agents use `PackageReview::RefreshPackages`, `InspectPackage {package}`, then
`ApprovePackage {package, review_id, reviewer, reason}` or
`RejectPackage {package, review_id, reviewer, reason}`. Inspection returns the full
submission, its review id, and either `verified_packages` or `inspection_error`.
Refresh and `PackagePage {page}` return at most 20 addressable summaries; no package
bytes or full signatures are returned in discovery. Registration and package
operations share one busy state, the same private-window requirement and owner
permission checks. Switching inspected requests clears feedback for the previous
request; registry state stays outside draft history.

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
    --namespace com.acme \
    --note "in-house team"
```

`--can-add-new` says this publisher may submit new entries. Without
it, they may only propose edits or deletion of their own rows —
useful for onboarding a publisher who's inheriting an existing
entry. `--namespace` (repeatable) is the reverse-DNS prefix the publisher's
package ids must start with; it is required and has no default. Naming Tilo's
own namespace `so.tilo` (or one inside it) also requires `--first-party`, so a
first-party grant is always deliberate:

```
tilo-registry-server add-publisher \
    --pubkey <publisher-hex> \
    --can-add-new \
    --namespace so.tilo \
    --first-party
```

**Upgrading an existing registry.** Rows written before namespaces existed
were read as `so.tilo`; they now own nothing. `serve` and `list-publishers`
log a warning naming each such key, and `list-publishers` shows it with
`namespaces = []`. For every key that genuinely publishes Tilo's own apps,
rerun `add-publisher --pubkey <hex> [--can-add-new] --namespace so.tilo
--first-party` (it replaces the row); give any other key its own namespace.
Until then those keys can still delete their entries, and moderation keeps
working.

### 4. Serve

```
tilo-registry-server serve --port 8443 --bind 127.0.0.1
```

The production OCI image uses the same interface and contains no TLS
termination or private signing material:

```text
registry.tilo.so/tilo/tilo-registry-server@sha256:<digest>
/usr/local/bin/tilo-registry-server \
  --data-dir /var/lib/tilo-registry serve --bind 0.0.0.0 --port 8443
```

It runs as UID/GID 65532 and supports a read-only root filesystem. Mount the
state directory read-write, mount `/tmp` as a small tmpfs, and probe
`GET /health` on port 8443. On main, t's `publish-registry-server` check
publishes only an immutable `sha-<full-source-revision>` tag and records the
deployable digest in its `receipt.json`.

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

Artifact URLs must be
under `https://apps.tilo.so/artifacts/`; a registry whose `base_url` is a
loopback address (`http://127.0.0.1:<port>`, `http://localhost:<port>`,
`http://[::1]:<port>`) also accepts its own `<base_url>/artifacts/`, where
`tilo-registry-server import-artifact <file.tapp>` hosts a file and prints
its URL.

A publisher without its own artifact hosting uploads the file to the registry
instead: `PUT /uploads/<blake3>.tapp` with the exact bytes, its public key in
`Tilo-Publisher` and, in `Tilo-Signature`, its Ed25519 signature over
`"tilo-registry-upload-v1\0"` followed by the lowercase BLAKE3 hex
(`registry_types::upload`). The registry accepts only an allowlisted
publisher's upload of a package that publisher signed, within the package's
size limit, and answers `201` with `{"blake3", "url"}`. The URL is
`https://apps.tilo.so/artifacts/<blake3>.tapp`, or the loopback prefix on a
local registry. Uploads stay private: review reads them from storage instead of
fetching the URL (`review_entry_bytes`), and `review approve` imports the
approved bytes as the artifact the URL then serves.

A bot entry's program is uploaded the same way, to
`PUT /uploads/<blake3>.program`, as the Shortcut's canonical JSON
(`tilo_automation::program`), at most 256 KiB. The registry refuses bytes that
are not exactly the canonical encoding (`422`), because the entry signs their
hash and a room stores the same bytes. The answer's `url` is
`https://apps.tilo.so/blobs/<blake3>`, which serves the program once review
approves a version that binds it. Pruning keeps a program blob while any
retained entry binds it.

`tilo-app-package bot-entry` builds a bot entry from a program file in any
formatting. It re-encodes the Shortcut canonically, derives the apps,
triggers and appearance the program needs, refuses `--connector
<id>@<min version>` lists that differ from the connectors its triggers use,
and with `--upload` sends the canonical bytes first:

```text
tilo-app-package bot-entry \
  --identity /run/secrets/app-publisher.json \
  --package-id so.tilo.bots.echo --version 1.0.0 \
  --action add --upload echo.json > request.json
```

A bot version is `{"version", "released_at_unix", "bot": {"program_url",
"program_blake3_hex", "program_size_bytes", "requires": {"connectors":
[{"package_id", "min_version"}], "apps", "appearance", "triggers"}}}`.
`requires` must equal what the program uses: the registry
checks it on submission and clients check it again on install. A bot is
never installed as an app; it goes into a room through that room's
`install-bot` action.

A newer request for a package replaces the one waiting for review and must
keep every version it lists, so a publisher releasing again before review
builds on its waiting entry. `GET /entries/<package id>/pending` returns that
request to its publisher only: the public key in `Tilo-Publisher`, the Unix
time in `Tilo-Signed-At`, and in `Tilo-Signature` an Ed25519 signature over
`"tilo-registry-pending-read-v1\0"`, the package id, a NUL and the time in
decimal (`registry_types::pending`), within five minutes of the registry's
clock. Any other caller, and a publisher with nothing waiting, gets `404`.

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
  "source_url": "https://code.tilo.so/example/example",
  "homepage": "https://example.invalid"
}
```

Clients resolve exact locale, then its base language, then English. Images use
the same fallback rule and distinguish `icon`, `screenshot`, and `banner`.

Each signed version binds the package root, exact final-file BLAKE3 and size,
platform family, architecture, minimum ISA and runtime ABI. The schema can add
future platform families without changing the signed entry shape. Five
families are accepted (`registry_types::target`):

| `platform` | `architecture` | `isa` | `runtime` |
|---|---|---|---|
| `tilo-os` | `x86_64`, `aarch64` | `x86_64`, `x86_64-v2`…`v4`, `aarch64`, `aarch64-v8.2`/`v8.4`/`v9` | optional, `tilo-runtime/<version>` |
| `linux` | `x86_64`, `aarch64` | the feature level it was compiled for, non-empty | **required**, the linked `tilo-runtime/<version>` |
| `windows` | `x86_64`, `aarch64` | the feature level it was compiled for, non-empty | **required**, the linked `tilo-runtime/<version>` |
| `macos` | `x86_64`, `aarch64` | the feature level it was compiled for, non-empty | **required**, the linked `tilo-runtime/<version>` |
| `android` | `x86_64`, `aarch64` | the feature level it was compiled for, non-empty | **required**, the linked `tilo-runtime/<version>` |

A native release must publish a distinct `.tapp` for both architectures. Pass
each tapp's target with `tilo-app-package entry --arch`. Linux, Windows,
macOS and Android versions sit beside Tilo OS versions in the same entry: version identity
is `(version, platform, architecture)`, and a Tilo OS client installs only
`tilo-os` versions.

**Linux, Windows, macOS and Android.** A `linux` build is for Linux desktops
other than Tilo OS. Those systems ship no Tilo toolkit, so the package is one
self-contained executable with the toolkit linked in; for `android` that is an
`.apk` (or an `.aab` for store upload, `registry_types::ANDROID_PACKAGE_EXTENSIONS`). Its signed `[target]`
names the executable as `module` (the GUI entrypoint must name the same path)
and the linked release as `runtime`, equal to `[package].runtime`
(`tilo_app_package::desktop::desktop_manifest`). Such a build may be up to
`MAX_DESKTOP_APP_TAPP_BYTES` (64 MiB), a distribution policy rather than a
design target; `tilo-app-package entry --platform linux|windows|macos|android`
binds it.

### 6. Moderator workflow

Publisher requests are stored in `pending/`, one retry-safe slot per
package. The owner reviews them locally; there is intentionally no
remotely callable approval endpoint.

```text
tilo-registry-server review list --json
tilo-registry-server review show so.tilo.example --json
tilo-registry-server review approve so.tilo.example \
    --review-id <review_id-from-show> \
    --reviewer alice --reason "capabilities and core journeys verified"
tilo-registry-server review reject so.tilo.example \
    --review-id <review_id-from-show> \
    --reviewer alice --reason "misleading subscription disclosure"
```

`show` and `approve` fetch every declared package version with strict
size and time bounds, refuse redirects and private-network destinations,
re-derive the content-addressed identity and publisher signature, and
compare the signed manifest's app id and version with the submission.
Their output includes filesystem, network, portal, and service requests.
Approval moves the exact signed metadata into snapshot staging; a moderator
cannot quietly rewrite a publisher's submission.

A bot entry has no package to fetch: its program was uploaded to this
registry before submission, and `submit` already refused it unless the bytes
hash and size-match, parse as a canonical, structurally valid program, and
need exactly the apps, connectors, triggers and appearance the version lists.
`show` reads the program from storage and prints its name, step count and
those needs; `approve` re-checks them and publishes the program at
`/blobs/<blake3>`. An id never changes kind: an app cannot be updated into a
bot or the reverse (409).

`show --json` includes a `review_id` for the entire signed pending request,
including its receipt time and both signatures. Pass it to either decision to
refuse a request replaced after inspection, before fetching packages or changing
registry state. Approval also rechecks the exact candidate under the registry
lock; rejection does the same and retains the reviewed bytes and literal feedback.
Rejecting an invalid request does not require its signature to become valid.
The flag is optional for existing CLI callers; agents should preserve and pass it.

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

The collector selects pending runtime providers before apps within the same
item bound, so a full page of deferred apps cannot keep its provider beyond the
cutoff. It reads a version from its own content-addressed store when it
already holds the bytes the publisher signed, and fetches only new versions.
A submission it cannot collect completely stays pending and out of the staged
batch; the others are staged, and the collector exits non-zero listing it.

The signer checks every collected request and package before selecting complete
requests whose new native versions have exact runtime providers in the resulting
catalogue. A missing runtime defers that whole request, preserving its pending
metadata, signatures and referenced package bytes. Independently installable
requests and runtime providers can still publish. An entirely deferred batch
reports `status = "deferred"` without signing or approving anything; a partial
promotion reports its promoted and deferred counts. Signature, authority, byte,
retention and recovery failures remain errors. The durable promotion journal and
staged inventory contain only the selected requests, so crash recovery repeats
the same verified transition. The next collector reconsiders pending requests.

To resolve a deferred request, publish its exact verified runtime or have the
original publisher replace its signed pending entry through normal publication.
Publisher reconciliation may retire an unpublished missing-runtime version only
after four newer offered releases of the same target and channel remain; it does
not rewrite package identities or remove published rollback history.

The store keeps each staged package once, as the payload blobs and thin form it
serves for thin installs plus a record of the package root, and rebuilds and
re-hashes the exact bytes on read. After every publish the signer deletes the
staged packages, thin packages and blobs that no published, quarantined,
pending or staged entry, retained history snapshot or served version index
references, such as the versions an entry dropped. `tilo-registry-server
prune-storage` does the same on demand; with `--dry-run` it only reports what
it would delete.

### Version retention

Publishers bound their entries: each signed entry keeps the newest versions of
every target (platform and architecture) and channel (release or pre-release),
20 by default (`registry_types::retention`), and drops the rest when the next
version is signed. The registry accepts a dropped version only when at least
`MIN_RETAINED_PER_CHANNEL` (4) newer versions of its target and channel stay,
or when the entry is at its 128-version limit and a newer version supersedes
it, so retention never removes a target's current version or its recent
rollback targets.

A dropped version leaves the catalogue through an ordinary signed snapshot:
the compact snapshot and version index stop listing it, `app rollback` stops
offering it, and the rollback, fork and freeze checks are unchanged. Its
artifacts stay while a retained history snapshot (which `rollback-snapshot`
may restore) or the previous version index (which a client one snapshot behind
may still fetch) lists it; the prune after that deletes them.

Retention is also what lets a device open an app without fetching a runtime:
a first open takes the newest retained version whose runtime build the device
already has (`app_cli::first_open_offer`). With the default of 20, an image or
per-user runtime from within the last 20 releases of an app opens it with no
runtime download; an older one downloads the runtime the newest version names.

On the device, a downloaded runtime build lives in
`$XDG_DATA_HOME/tilo-runtime/<version>/` only while something needs it
(`runtime_bundle::gc`). After every `tilo-install install`, `update` and
`uninstall`, and on `tilo-install gc`, a build is removed unless an installed
package (the user's or the image's) names it as its `runtime` or among its
`optional-runtimes`, it is pinned (`tilo-install runtime pin`, or installed
with `--pinned`), it was installed within the last hour (an app's first open
installs its runtime and then the app, in two steps), or a running app has it
mounted. A build leaves its version's directory in one rename before any file
is deleted, so a collection stopped halfway never leaves a build a launch
would pick. Nothing runs on a timer.

A launch holds what it runs on from before it looks at it until its sandbox
exits: a shared `flock` on `.launches/<version>` in the runtime root for each
per-user runtime build, and on `leases/<package id>` in the app store for its
payload tree. Removal takes the same file exclusively without waiting, so an
app opened while it is being updated keeps the build it chose, payload and
runtime, and the collection after the launch ends takes them. A launcher that
dies releases its holds with its descriptors. The app host's own installs and
removals run the same collection as `tilo-install`, which also removes the
staging a crashed install left in the store's `tmp/`.

`prune-storage --dry-run --keep-releases N --keep-prereleases M` lists the
published versions that policy drops and what pruning deletes once they are
gone, without changing anything.

Both seed files are exactly 32 secret bytes encoded as 64 hex characters. The
root seed exists only in the bootstrap/manual authorization environment. The
serving process receives neither private key. The collector cannot forge a
stage because the signer treats its JSON as untrusted and independently checks
the content-addressed bytes. The signer has no network path from which to fetch
or substitute content.

Every successful signing also retains the exact signed JSON under
`snapshots/<sequence>-<hash>.json` before advancing `published-snapshot.json`.
These immutable files are the channel history and share the data directory's
backup policy. Roll back catalogue contents by publishing a new, higher
sequence from one of them:

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

### Bounded snapshot history

Clients never read the history. Native and browser clients fetch only the
current snapshot from `/entries` and keep a checkpoint of the last sequence
and hash they accepted: they refuse a lower sequence (rollback), another
snapshot at the same sequence (fork), and anything expired (freeze, bounded
by the snapshot lifetime). These checks need nothing but the current
snapshot, so pruning history cannot weaken them.

The history serves the registry itself: `rollback-snapshot` restores from it,
and its members stop the channel from signing a sequence twice. Each member
is the whole catalogue, so the history is kept to the newest
`SNAPSHOT_HISTORY_RETAINED` (16) members. Every signing command, the publisher
and `prune-storage` compact it under the registry lock:

- The members to prune, and the oldest one kept, must form one hash chain
  from the previous compaction record (or the first member). A broken chain
  stops compaction without deleting anything.
- `snapshots/compacted.json` first records the highest pruned sequence and
  its hash. The oldest member kept signs that hash as its predecessor, so the
  signed chain still commits to every pruned snapshot, and the record can be
  checked against it.
- The registry refuses to archive or publish any sequence at or below the
  record, and refuses to restart the channel at sequence 1 when the
  published pointer is lost. Restore `published-snapshot.json` from a backup
  instead.

A client last synced at a sequence that has since been pruned still verifies
the current snapshot: its sequence is higher and its signature, freshness and
publisher checks hold on their own. A replay of any older snapshot, pruned or
not, stays below the checkpoint and is refused. Snapshots older than the
retained window can only be restored from backups.

### Compact snapshots and version indexes

Every entry keeps up to 128 versions, and every client fetches the snapshot
at least daily, so the full snapshot grows with each release. Each signing
command therefore also signs a compact form (protocol 4,
`registry_types::compact`) with the same key, owner or channel:

- Per entry it keeps only the newest release and the newest pre-release of
  each exact target (`newest_versions`), in their original order. Any
  client's newest installable version on either channel is among them.
- Per entry it signs the BLAKE3 hash and size of a version index,
  `version-indexes/<blake3>.json` served at `/indexes/<blake3>.json`: the
  complete publisher-signed entry with every retained version. So the signed
  snapshot still commits to every older version.
- It signs `snapshot_hash_hex`, the hash of the full snapshot at the same
  sequence. That hash is the snapshot's identity: the next snapshot names it
  as predecessor and clients checkpoint it whichever form they fetched, so
  rollback, fork and freeze checks are unchanged and a client may switch
  forms freely.

Clients fetch an index only to install or roll back to an older version
(`app install <app> --version <v>`, `app rollback <app> [--to <v>]`).
`verify_version_index` checks its size and hash against the signed values,
that it is the same package, that the publisher's signature over the
complete entry verifies, and that the entry reduces exactly to the one in
the snapshot. A compact snapshot cannot carry publisher signatures, which
cover complete entries; its own signature, the index check and the package's
signature on install take their place.

The server serves the compact form only while it verifies and is the form of
the current full snapshot; otherwise `?format=compact` gets the full one.
Publishing keeps the version indexes of the current and the previous compact
snapshot, so a client one snapshot behind can still fetch its index.

**Rollout.** `/entries` without a query keeps serving the full snapshot, so
released clients work unchanged. The native client (Store and `app`) and the
browser client ask for `?format=compact` and accept either form, also from
older registries. Once released images carry those clients, the default can
change to compact; until then the full form stays the default and remains
available.

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

### Edge cache for apps.tilo.so

Every content-addressed route above, and the generic-package files under
`/artifacts/<owner>/generic/...` the release pipeline publishes, is immutable
by construction: its URL names its bytes. The Tilo deployment puts Cloudflare
in front of Caddy, whose configuration lives with the deployment, not in this
repository. For the edge to keep these objects:

- **One `Cache-Control`.** Forgejo's package route adds
  `Cache-Control: private, max-age=300` to every file; the proxied response must
  carry only `public, max-age=31536000, immutable`. In Caddy, inside the
  `reverse_proxy` of the `/artifacts/*` route that proxies to Forgejo,
  `header_down Cache-Control "public, max-age=31536000, immutable"` (setting
  replaces Forgejo's value), and no site-level `header Cache-Control` on that
  route: one applied before the proxy is added to, not replaced by, the
  upstream header, which is how two arrive today.
  The registry server already sends exactly that header for `/blobs/*`,
  `/thin/*`, `/indexes/*` and its own `/artifacts/<blake3>.tapp[.zst]`.
- **Eligible for cache.** `/blobs/<blake3>` has no file extension and `.tapp`
  is not in Cloudflare's default cached set, so a Cache Rule must make
  `/artifacts/*`, `/blobs/*`, `/thin/*` and `/indexes/*` "Eligible for cache"
  with "Use cache-control header if present". Leave `/entries*`, `/registry`
  and every non-GET out of it: the snapshot revalidates by ETag.
- **No transformation.** Twins are `application/zstd`; Cloudflare's own
  compression must not touch them (it does not by default).

Check it with `tools/ci/probe-registry-edge-cache.sh <tapp-url> <blob-url>`:
it fetches each URL twice and fails unless the second answer is a
`cf-cache-status: HIT` with exactly one `Cache-Control`.

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
        <sequence>-<hash>.json # newest signed channel history, bounded
        compacted.json         # highest pruned sequence and its hash
    published-snapshot.json    # atomic current channel pointer
    published-compact-snapshot.json # compact form of the current snapshot
    version-indexes/
        <blake3>.json    # complete signed entries the compact form names
    payload-blobs/
        <blake3>         # payload blobs of reviewed packages, and approved bot programs
    thin-packages/
        <root>.tapp      # thin form of each reviewed package
    artifacts/
        <blake3>.tapp    # full packages hosted with import-artifact
    uploads/
        <blake3>.tapp    # publisher package uploads awaiting review; private
        <blake3>.program # publisher bot programs awaiting review; private
```

All writes are atomic-rename (`.tmp` → real path), so
`tar -czf backup.tgz data-dir/` while the server is running is
safe — the worst case is that you capture a subset of
in-flight changes; nothing gets torn.

For a restore, drop the same tarball back in place and start
`tilo-registry-server serve`. There's no daemon-side state
outside the data-dir — the server is stateless across restarts.

### Entry format cut-over

Entries signed under the earlier domain, before `kind` and version
`artifact`s, do not verify and are not read. A registry moves to the
current form by archiving its data directory, starting empty, and
republishing every app through the normal release workflow, then
publishing a signed snapshot. Devices whose app installer predates the
change cannot read the new catalogue until their OS update brings one that
can.

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
