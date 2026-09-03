# Local model catalog releases

`qos-model-catalog` is the only supported compiler for the catalog consumed by
`qos-model-manager`. It turns a declarative JSON source plus local release
artifacts into two image inputs:

- `catalog.json`: model releases, signed runtime-extension manifests, artifact
  receipts, and a domain-separated Ed25519 signature;
- `keys.json`: the public verification key indexed by release key id.

The signing seed is an input only. The compiler requires its file to be mode
`0600` (or stricter), reads exactly 32 bytes encoded as lowercase hexadecimal,
and never writes that seed into either output.

```sh
cargo run -p local-model-service --bin qos-model-catalog -- \
  --source release/local-models.json \
  --signing-key /run/secrets/qos-model-release-key \
  --catalog-out dist/etc/qos/models/catalog.json \
  --trust-out dist/etc/qos/models/keys.json
```

The source must point at regular files already fetched by the release job.
Their byte count and SHA-256 are recomputed; declared receipts are comparisons,
not trusted input. Model revisions must be exact 40- or 64-character lowercase
commit hashes and the immutable revision must occur in the HTTPS artifact URL.
Runtime container identities must end in `@sha256:<64 lowercase hex>`. Floating
branches, tags, `latest`, redirects encoded as query strings, symlinks, empty
artifacts, and production `.invalid` URLs are rejected.

The catalog signature covers:

```text
"qos.local-model.catalog.v1" || 0x00 || compact JSON(payload)
```

The checked-in `tests/fixtures/catalog-source.example.json` is explicitly
`"production": false`; its tiny text artifacts exercise deterministic tooling
and are not model weights, a runtime, or approved upstream release pins.
