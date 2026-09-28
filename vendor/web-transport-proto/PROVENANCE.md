# web-transport-proto provenance

- Package: `web-transport-proto` 0.6.2
- Registry checksum:
  `1a81bcc79bf48ad0b819bfa51ec28524ee7c356056d43c316727d6b505dcc71e`
- Repository: `https://github.com/moq-dev/web-transport`
- Upstream revision: `7dcaac76c40f5228f1e973cc4256818e0c887271`
- Upstream package path: `rs/web-transport-proto`
- Clean upstream package tree SHA-256:
  `233275f6ece897d7b73649242d53a314a2f1510f5b0ef258030dac8ab436202c`
- License: MIT OR Apache-2.0
- MIT license SHA-256:
  `a1073dd45800c01420a85b3cf3ed705d53162b3cc05b461d9336960c812716e3`
- Apache-2.0 license SHA-256:
  `cfc7749b96f63bd31c3c42b5c471bf756814053e847c10f3eb003417bc523d30`

Local patch: `src/connect.rs` retains the exact received HTTP/3 `:authority`
and `:path` pseudo-header values alongside the existing normalized `Url` view.
Read-only accessors let a shared ingress compare the raw values before route
selection or MoQ SETUP parsing. Explicit outgoing overrides support adversarial
wire tests. `src/qpack.rs` rejects duplicate pseudo-header names before its
map representation could discard them; duplicate ordinary headers retain the
upstream last-value behavior. `src/qpack.rs` also gains `Headers::decode_fields`, which returns every field
line in wire order with duplicates retained (split `cookie` crumbs), and
`src/lib.rs` re-exports `Headers`/`DecodeError` as `QpackHeaders`/
`QpackDecodeError` so the host's shared HTTP/3 dispatcher can decode plain
request header blocks and encode response headers. The URL parser and all
other protocol behavior remain upstream.

The workspace root selects this copy with its reviewed Cargo patch. Keep any
future source change explicit here; do not edit manifests or the lockfile
implicitly.
