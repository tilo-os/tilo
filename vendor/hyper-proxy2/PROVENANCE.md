# hyper-proxy2 0.1.0

Source: the published `hyper-proxy2-0.1.0.crate`, upstream commit
`0d2dfbcd6c6af83046d56f688f5110c37cc65140`.
Archive SHA-256:
`9043b7b23fb0bc4a1c7014c27b50a4fc42cc76206f71d34fc0dfe5b28ddc3faf`.
The upstream MIT licence is preserved in `LICENSE-MIT.md`.

Tilo changes to both manifests:

- Hyper-Rustls 0.27.7, Tokio-Rustls 0.26.4 and native-certs 0.8.3 reuse the
  workspace's current Rustls/ring stack and fixed-path trust loader.
- Explicit ring/TLS features avoid the newer dependencies' AWS-LC defaults.
- The unused direct webpki 0.22 dependency/feature edge is removed.
- A dev-only rcgen dependency generates local certificates for a CONNECT/TLS
  regression test. It verifies rejection of an untrusted certificate and a
  trusted certificate for the wrong hostname, plus a valid handshake.

The proxy runtime source/API is unchanged; `src/lib.rs` only adds the test
module. Upstream optional TLS backends/defaults are retained; Music's core
dependency disables defaults and chooses Rustls. No insecure verifier, trust
fallback or certificate-policy exception is introduced.

Remove the patch once upstream ships the current Rustls dependency generation.

The archive’s standalone Cargo.lock is omitted: these dependencies use Tilo’s
committed workspace lock, and upstream standalone locks describe optional TLS
backends that are outside Tilo’s allowed runtime dependency graph.
