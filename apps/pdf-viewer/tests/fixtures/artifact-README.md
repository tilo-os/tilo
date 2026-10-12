# Frozen document inputs

Original bytes are preserved in the immutable package store and embedded or opened from verified build output. This retains the existing producer/compression/font-stream inputs and security cases. The original [repository license](https://code.tilo.so/api/packages/tilo/generic/source-evidence/0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0/license.txt) was archived unchanged; existing notices and generator sources remain in the repository. Source snapshot: `b7ea536d18fef14cc95b3d94d400d43a601dad4e`.

Prewarm with `python3 tools/test-assets.py apps/pdf-viewer/tests/fixtures/artifacts.json`. Use `TILO_TEST_ASSETS_OFFLINE=1` and `CARGO_NET_OFFLINE=true` for offline builds with that cache. `TILO_TEST_ASSET_CACHE` selects a different cache. Missing or corrupt bytes fail preparation. Apps never download these fixtures at runtime.

| Original input | Bytes | SHA-256 |
| --- | ---: | --- |
| [sample.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/4a14d5e23f32abdda14329a2973ef33e02fd01197733bba38e4f40426e7983d4/sample.pdf) | 3735 | `4a14d5e23f32abdda14329a2973ef33e02fd01197733bba38e4f40426e7983d4` |
