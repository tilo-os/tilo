# Trust: publisher identities, attestors, revocation

Tilo packages are content-addressed and signed. This document covers what
that means for publishers (how identities work, when to get an attestor
to co-sign) and for users (how to configure trust).

## Identities

Every package is signed by an Ed25519 keypair. The public key goes in
the manifest's `[publisher]` block and into the signed root:

```toml
[publisher]
identity  = "ed25519:c4c627b57a518bc318c2854662b44f741345f6d55a45eb059d3b384048789e39"
signature = ""                    # empty at author time; filled by tilo package pack / sign
```

The 32-byte hex is the raw public key. `tilo package pack` without
`--identity` **generates a fresh keypair each pack** and overwrites the
manifest's `identity` field with the fresh public key — that is the
"self-signed" flow every walkthrough here demonstrates.

For repeatable publishing, use `tilo package pack --identity <file.json>`.
The identity file is:

```json
{ "secret_key_hex": "<32-byte hex — Ed25519 signing-key seed>" }
```

Store it out-of-band; treat it as a signing secret. The corresponding
public key comes out of Ed25519's key derivation and lands in the
`.tapp` on every pack.

## Threshold signing

Under the hood, `SigningIdentity` wraps EITHER a plain `SigningKey` or a
1-of-1 `ThresholdIdentity`. FROST-based threshold Ed25519 signatures
verify identically to plain Ed25519 signatures at the wire, so a
k-of-n aggregated signature just IS the signature in the `.tapp`. The
aggregation flow itself is driven outside the `pkg` crate; call
`pack_with_signature` with the aggregated bytes if you have your own
signing service.

## The signed root

The bytes signed are:

```text
manifest_hash[32] || payload_root_hash[32] || publisher_identity[32]
```

96 bytes → Ed25519 signature → 64-byte signature. The result plus the
three inputs is the 173-byte `signed-root` blob. Its BLAKE3 hash IS the
`PackageId` — the `tapp:<hex>` URL you see in `tilo package inspect`.

Consequences:

- Any byte change in the manifest or the payload flips both the
  intermediate hash and the `PackageId`.
- A re-sign with a new signature flips the `PackageId` (the signature is
  part of what's hashed).
- Two identical `pack --identity` runs produce the same `PackageId` only
  if the manifest's `publisher.identity` was already correct (else pack
  overwrites it, changing the manifest bytes, changing everything
  downstream).

## The attestor registry

Users have a per-machine `~/.config/tilo/trusted-attestors.toml`:

```toml
[[attestor]]
id         = "tilo-curated/2026.07"
pubkey_hex = "…"
scope      = "all"                        # or "mime:<a>,<b>" / "prefix:<a>,<b>"
```

Fields:

- `id` — a human-readable string. Must match the `attestor` field in a
  package's `[[trust.attested_by]]` row for the signature to count.
- `pubkey_hex` — the attestor's Ed25519 public key (64 hex chars).
- `scope` — what the attestor is trusted for:
  - `"all"` — every package they sign.
  - `"mime:<a>,<b>"` — only packages whose `[providers].mime` names any
    of the listed types.
  - `"prefix:<a>,<b>"` — only packages whose `[package].id` starts with
    one of the listed prefixes.

Manage from the CLI. Verified end-to-end:

```
$ tilo app trust list
(no trusted attestors)

$ tilo app trust add \
    --id "test-attestor/2026" \
    --pubkey "0000000000000000000000000000000000000000000000000000000000000001" \
    --scope "all"
trust: added test-attestor/2026 (scope: all)
  file: /tmp/tilo-install-demo-config/tilo/trusted-attestors.toml

$ tilo app trust list
id                        scope         pubkey
test-attestor/2026        all           0000000000000000000000000000000000000000000000000000000000000001

$ tilo app trust remove test-attestor/2026
trust: removed test-attestor/2026
```

## Attaching attestations to a package

Every attestation is a row in `[[trust.attested_by]]`:

```toml
[[trust.attested_by]]
attestor  = "tilo-curated/2026.07"     # matches the trusted-attestor id
claim     = "deadbeef"                # opaque hex; attestor-specific
signature = ""                        # 64-byte Ed25519 signature over the preimage below
```

The attestor's signature covers a domain-separated preimage:

```text
b"tilo-app-package-attest-v1\0" || payload_root_hash[32] || publisher_pubkey[32]
```

(82 bytes total.) `payload_root_hash` is the BLAKE3 of the payload
Collection blob — the same one you can pull out of `tilo package inspect`.
`publisher_pubkey` is the raw 32 bytes from `[publisher].identity`.

Note: the signature is NOT over the `PackageId`. The `PackageId`
transitively covers the manifest — which includes the attestation
signatures themselves — so signing over it would create a fixed-point
loop. Using the payload root + publisher key means the attestation
binds to "this payload, signed by this publisher" and stays stable when
additional attestations are added to the manifest.

For k-of-n multi-attestor policies, add multiple `[[trust.attested_by]]`
rows. `verify_with_trust` returns on the first trusted-attestor match;
strict consumer policies can walk the manifest and check each row
independently.

## Co-signing a local package

Use separate publisher and attestor identities generated by `tilo package keygen`.
Keep both identity JSON files private; the command prints only their public keys.
After packing the complete app with the publisher identity, co-sign it through:

```sh
tilo package attest --publisher-identity publisher.json \
  --attestor-identity attestor.json --attestor owned-fixture \
  --scope prefix:com.example.owned-fixture original.tapp -o attested.tapp
```

The command verifies the original publisher signature and every payload blob,
checks the intended scope, and refuses incomplete inputs or existing outputs.
It preserves payload bytes, the payload root and the publisher identity. Adding
an attestation changes the manifest and therefore creates a new publisher-signed
package root; it does not grant any recipient trust by itself. The recipient
separately enrolls the public attestor key through ordinary `tilo app trust
add` with the same narrow prefix. Its scope and revocation policy remain
independently authoritative. Existing attestation rows have no expiry field.

## The install-time policy: `--require-attestor`

`tilo app install <file>` follows the app host's rule: a package a
publisher you trust signed installs; a self-signed or unknown publisher is
named as unverified and asked about once, and with nobody to ask (a script)
it is refused unless `--yes` is given. A revoked publisher is refused
whatever is given. `--allow-self-signed` means `--yes` there; with
`--require-attestor` an unverified package is refused without asking, unless
`--yes` is given.

The installer underneath, `tilo-install install`, warns on self-signed
packages and proceeds:

```
$ tilo-install install /tmp/tilo-hello-demo/tilo-hello.tapp
tilo-install: WARN — package self-signed by ed25519:c4c627b57… (no trusted-attestor signature); use --require-attestor <id> to demand an attestation
installed: so.tilo.hello 0.1.0
```

Two flags tighten this:

- `--require-attestor <id>` — refuse the install unless the package
  ships a valid signature from the named attestor AND that attestor is
  in the trust registry.
- `--allow-self-signed` — permit a self-signed package as a fallback
  when `--require-attestor` doesn't match.

Combined behaviour, verified end-to-end:

```
$ tilo-install install --require-attestor "test-attestor/2026" /tmp/tilo-hello-demo/tilo-hello.tapp --force
tilo-install: policy refused: --require-attestor "test-attestor/2026" and package is self-signed (use --allow-self-signed to override)

$ tilo-install install --require-attestor "test-attestor/2026" --allow-self-signed /tmp/tilo-hello-demo/tilo-hello.tapp --force
tilo-install: --require-attestor "test-attestor/2026" unmatched; --allow-self-signed permits self-signed publisher ed25519:c4c627b57…
installed: so.tilo.hello 0.1.0
```

Use in a system-management context:

```bash
# strict policy for a fleet: every install must be signed by our attestor
tilo-install install --require-attestor "acme-corp/prod" acme-app.tapp
```

Or, in a locked-down environment, drop the `--allow-self-signed`
fallback entirely.

## Revocation

`~/.config/tilo/revoked-keys.toml`:

```toml
[[revoked]]
pubkey_hex = "deadbeef…"
reason     = "compromised 2026-07-05"
```

Add via CLI:

```
$ tilo app trust revoke deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef \
    --reason "test revocation"
revoked: ed25519:deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef
  reason: test revocation
  file: /tmp/tilo-install-demo-config/tilo/revoked-keys.toml

$ tilo app trust revoked
pubkey                                                              reason
deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef    test revocation
```

Revocation is enforced at BOTH install and run time. If a publisher is
revoked AFTER a package is installed, the next `tilo-install run` still
refuses:

```
tilo-install: REFUSED — installed package's publisher is revoked: <reason>
```

This is why the launcher re-reads the revocation list at every launch —
not just at install. There is no "trust the install-time verification
forever" cache.

## Sharing keys across a fleet

The two config files (`trusted-attestors.toml`, `revoked-keys.toml`) are
plain TOML and can be provisioned by any config-management tool — a
first-run script, Ansible, systemd-tmpfiles, etc. There is no
mutable-network component in the model today; every machine reads its
own on-disk trust config.

## `tilo package verify` and trust

Standalone verification (without touching the install-time registry):

```
$ tilo package verify /tmp/tilo-hello-demo/tilo-hello.tapp
ok: tapp:b8bde92e2352dd7a3b3e094968272c42d51196697a5cec17f36e3595a55a71d3 verifies and publisher is trusted
```

By default the embedded publisher key is auto-trusted (self-signed
convenience). Pass `--trusted-key <hex>` (repeatable) to demand a
specific publisher:

```
$ tilo package verify --trusted-key <some-key> /tmp/tilo-hello-demo/tilo-hello.tapp
signature-ok-but-untrusted: tapp:… (publisher ed25519:c4c627b5…)
```

Exit code:

- `0` — verified and publisher is on the `--trusted-key` list (or auto-
  trusted when no list is set).
- `2` — cryptographically valid but the publisher isn't trusted.
- `1` — cryptographic failure (broken signature, wrong hash).

Use `verify --trusted-key` in CI pipelines that gate `tilo-install`
without maintaining a persistent trust registry.

Next: [`cross-target.md`](./cross-target.md) — shipping the same source
to Linux, macOS and the web.
