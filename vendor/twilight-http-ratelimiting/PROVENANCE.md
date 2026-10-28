# twilight-http-ratelimiting provenance

- Package: `twilight-http-ratelimiting` 0.17.1
- Registry checksum:
  `0515b0c30814068a7540fcb5f58b634259ca453fa335d42c3b2c8f2b06ac6a59`
- Repository: `https://github.com/twilight-rs/twilight`
- Upstream HEAD inspected: `46460a801c10324341bf48b1719ecd1c36b7c6cd`
- License: ISC. The published archive carries no licence text; `LICENSE.md`
  is the repository root's file at the inspected revision, byte for byte.

Local patch, `src/actor.rs`: the backport of upstream commit
`71aa8dd629b5fbc7637ff1a2c62ecee7a48e3dab` ("fix(http-ratelimiting): remove
invalid assert"). A bucket's reset timer can fire while a call in it is in
flight, when the bucket still had calls left; the release's debug assertion
then panicked the limiter's task, failing every later call. Everything else is
the published source. Remove the copy once a release carries the fix.

The workspace root selects this copy with its reviewed Cargo patch.
