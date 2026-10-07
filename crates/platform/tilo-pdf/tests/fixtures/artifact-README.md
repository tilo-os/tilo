# Frozen document inputs

Original bytes are preserved in the immutable package store and embedded or opened from verified build output. This retains the existing producer/compression/font-stream inputs and security cases. The original [repository license](https://code.tilo.so/api/packages/tilo/generic/source-evidence/0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0/license.txt) was archived unchanged; existing notices and generator sources remain in the repository. Source snapshot: `b7ea536d18fef14cc95b3d94d400d43a601dad4e`.

Prewarm with `python3 tools/test-assets.py crates/platform/tilo-pdf/tests/fixtures/artifacts.json`. Use `TILO_TEST_ASSETS_OFFLINE=1` and `CARGO_NET_OFFLINE=true` for offline builds with that cache. `TILO_TEST_ASSET_CACHE` selects a different cache. Missing or corrupt bytes fail preparation. Apps never download these fixtures at runtime.

| Original input | Bytes | SHA-256 |
| --- | ---: | --- |
| [arabic.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/ef31f7b460ff3312d249b35e64407a30496b8f3edb53d5965191b2024c55913c/arabic.pdf) | 7551 | `ef31f7b460ff3312d249b35e64407a30496b8f3edb53d5965191b2024c55913c` |
| [cjk.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/a0588bd15cc3f229c20685f1a4231f00548a2dd0c9fe01ad769ecd90c1c57b5c/cjk.pdf) | 8417 | `a0588bd15cc3f229c20685f1a4231f00548a2dd0c9fe01ad769ecd90c1c57b5c` |
| [columns.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/50ad66e3611dcaeb51548399d74ceb74d8591525a9d4573ffba9f015bf1339f4/columns.pdf) | 12359 | `50ad66e3611dcaeb51548399d74ceb74d8591525a9d4573ffba9f015bf1339f4` |
| [form.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/e4fea4dbe6cdf79b0ed40b8915343d82d691a0f56d07de70d3734db46cd08ad1/form.pdf) | 1730 | `e4fea4dbe6cdf79b0ed40b8915343d82d691a0f56d07de70d3734db46cd08ad1` |
| [latin.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/fba23d4b1682db9f9ab1cb8fc3c3439ae2271b1f1290d561dd830a2d971ee873/latin.pdf) | 9278 | `fba23d4b1682db9f9ab1cb8fc3c3439ae2271b1f1290d561dd830a2d971ee873` |
| [ligatures.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/29a6de888b27ee65569d82bf7714950dc00464c68c8033418528c5e9293eda84/ligatures.pdf) | 7363 | `29a6de888b27ee65569d82bf7714950dc00464c68c8033418528c5e9293eda84` |
| [links.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/3b98da122fca7f48a1a9c00e3f7fa52dde53f3f70f0151e760e10dead89e7649/links.pdf) | 13992 | `3b98da122fca7f48a1a9c00e3f7fa52dde53f3f70f0151e760e10dead89e7649` |
| [multipage.pdf](https://code.tilo.so/api/packages/tilo/generic/source-evidence/9adb7ae87c38e32ec6c1dac517dc22381e9b5cd8ea2b286927a13dd302a7455a/multipage.pdf) | 15876 | `9adb7ae87c38e32ec6c1dac517dc22381e9b5cd8ea2b286927a13dd302a7455a` |
