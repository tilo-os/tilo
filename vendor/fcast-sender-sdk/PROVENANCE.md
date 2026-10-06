# fcast-sender-sdk provenance

- Published crate: `fcast-sender-sdk` 0.3.0 (MIT).
- Source archive: https://static.crates.io/crates/fcast-sender-sdk/fcast-sender-sdk-0.3.0.crate
- Archive SHA-256: `f287d576e0dff57869e24c00468747821ab6a8a2db8e9093d29d12b7688a9bb5`, matching the original committed Cargo.lock.
- Upstream revision: `f00ed90d75b8cb5f80d2137352ad5f2fb1ce296c`.
- Upstream path: `sdk/sender/fcast-sender-sdk`.
- Full license: `LICENSE`, copied from that exact upstream revision. No protocol authority certificates or private keys are included.

Local changes:

1. Use edition 2024 and disable publishing. Retain the published source bytes outside the patched Cast module. Format the patched Cast module and its new transport/tests.
2. Replace the Cast transport verifier's unconditional TLS 1.2/1.3 signature acceptance with Rustls's ring verification against the presented certificate. Advertise only the provider's supported schemes. Preserve upstream's self-signed transport certificate policy; this does **not** authenticate a receiver identity.
3. Bound TLS negotiation to five seconds; `Quit` and command-channel closure drop the pending handshake and transport. Keep the same connection, controller, resource lifecycle and FCast implementation.
4. Add real TLS tests with ephemeral RSA certificate/key generation, mismatched signing keys, cancellation, timeout and disconnect. OpenSSL absence is a test failure, never a skip. The projector integration target includes this exact production transport source so its normal CI tests execute these regressions; cancellation is a generic future and introduces no test command or controller implementation.

The workspace patch selects this exact copy. Cargo.lock removes only the SDK registry source/checksum and records the projector test target's two already-locked Rustls dependencies; all versions and other dependency bindings stay unchanged. DeviceAuth challenge verification requires a separately reviewed dedicated Cast authority and revocation policy; neither ambient WebPKI nor Tilo device authorities may substitute for it.
