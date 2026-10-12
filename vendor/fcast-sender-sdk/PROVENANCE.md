# fcast-sender-sdk provenance

- Published crate: `fcast-sender-sdk` 0.3.0 (MIT).
- Source archive: https://static.crates.io/crates/fcast-sender-sdk/fcast-sender-sdk-0.3.0.crate
- Archive SHA-256: `f287d576e0dff57869e24c00468747821ab6a8a2db8e9093d29d12b7688a9bb5`, matching the original committed Cargo.lock.
- Upstream revision: `f00ed90d75b8cb5f80d2137352ad5f2fb1ce296c`.
- Upstream path: `sdk/sender/fcast-sender-sdk`.
- Full license: `LICENSE`, copied from that exact upstream revision. No private keys are included. The protocol-only DeviceAuth data and schemas additionally retain Open Screen’s BSD-3-Clause license in `src/chromecast_auth/LICENSE.OpenScreen`.

Local changes:

1. Use edition 2024 and disable publishing. Retain the published source bytes outside the patched Cast module. Format the patched Cast module and its new transport/tests.
2. Replace the Cast transport verifier's unconditional TLS 1.2/1.3 signature acceptance with Rustls's ring verification against the presented certificate. Advertise only the provider's supported schemes. Preserve upstream's self-signed transport certificate policy; this does **not** authenticate a receiver identity.
3. Bound TLS negotiation to five seconds; `Quit` and command-channel closure drop the pending handshake and transport. Keep the same connection, controller, resource lifecycle and FCast implementation.
4. Add real TLS tests with ephemeral in-memory P-256 certificate/key generation, mismatched signing keys, cancellation, timeout and disconnect. Certificate generation uses the existing locked rcgen/ring stack, without a subprocess or filesystem private keys. The production loopback journey separately covers an RSA transport leaf. The projector integration target includes this exact production transport source so its normal CI tests execute these regressions; cancellation is a generic future and introduces no test command or controller implementation. Its dependency bindings are explicit in both callers, so the compiler and dependency scanner can observe the shared-source imports.

5. Authenticate the selected screen receiver on the same TLS connection before CONNECT or any media URL. Consume a fresh OS-random nonce once; verify SHA-256 proof, certificate path, capability and signed revocation with locked Rustls/WebPKI. Cancel, timeout, malformed replies and expired authentication fail closed. The connection cannot outlive its TLS leaf, verified identity path or signed CRL.
6. Embed only protocol-private public Cast/Eureka receiver anchors and the separate Cast CRL anchor as literal byte arrays in `src/chromecast_auth/authorities.rs`. These UTF-8/LF source constants preserve the original DER bytes and require no decoder or binary-source exception. No OS/browser/user/Tilo collaborator trust setting or persistent pairing grant changes. Factory identity does not establish user ownership or capture consent.
7. Build the `[lib]` as an `rlib` only. Upstream's `cdylib` and `staticlib` serve its UniFFI bindings, which Tilo does not build, and Cargo leaves the hash out of a path package's library filenames when it builds a dylib or cdylib, so two builds of this crate in one Cargo run would overwrite each other's `libfcast_sender_sdk.rlib` and break dependents with E0460/E0463.

Open Screen revision: `1c526caf732881bd0aa1ab9479c36861bb225ba6`, obtained from Google's repository using certificate-validated HTTPS. This is repository provenance, not an independently verified Google-signed trust bundle or commit signature. Source links, DER SHA-256 pins and compatibility policy are recorded in `docs/architecture/continuity-interop.md`. Retained `cast_channel.proto` and `revocation.proto` come from that revision; `proto.rs` implements their relevant wire fields. The three DER hashes are:

- Cast: `809af14700b3fe2611ad597eb1584d6354313b64ccdb3390f097fa3e5826d6ca`
- Eureka: `caf6d1e37b532203e01a76bf07187bb731ccd38801565ab2211a2c0ab7f3bc46`
- CRL: `8b77c60291f9d62764613083f49750b1c5caac2d9efbe5a28edf301f9d84831d`

The workspace patch selects this exact copy. Cargo.lock records the SDK and production-source test dependencies using existing locked versions; it is not freshly resolved. Synthetic test authorities remain private to isolated tests. No runtime authority loader, TOFU or ambient WebPKI substitution is provided. The historical unauthenticated loopback fixture now qualifies production rejection by default; its explicit legacy mode only documents the earlier transport journey.
