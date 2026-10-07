# Frozen document inputs

Original bytes are preserved in the immutable package store and embedded or opened from verified build output. This retains the existing producer/compression/font-stream inputs and security cases. The original [repository license](https://code.tilo.so/api/packages/tilo/generic/source-evidence/0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0/license.txt) was archived unchanged; existing notices and generator sources remain in the repository. Source snapshot: `b7ea536d18fef14cc95b3d94d400d43a601dad4e`.

Prewarm with `python3 tools/test-assets.py crates/doc/fixtures/artifacts.json`. Use `TILO_TEST_ASSETS_OFFLINE=1` and `CARGO_NET_OFFLINE=true` for offline builds with that cache. `TILO_TEST_ASSET_CACHE` selects a different cache. Missing or corrupt bytes fail preparation. Apps never download these fixtures at runtime.

| Original input | Bytes | SHA-256 |
| --- | ---: | --- |
| [lo-annotations.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/b6dfd11a7aeb209a8bc271d93c7ee4690f3cd2c1fadbaccbe212f9bcb5c7521f/lo-annotations.docx) | 8208 | `b6dfd11a7aeb209a8bc271d93c7ee4690f3cd2c1fadbaccbe212f9bcb5c7521f` |
| [lo-annotations.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/01601aa8e1cb7ed87f2b1ac909b26bc6bbaa89407729e6f81fa3614a2896096c/lo-annotations.odt) | 11753 | `01601aa8e1cb7ed87f2b1ac909b26bc6bbaa89407729e6f81fa3614a2896096c` |
| [lo-drawings.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/c4e6188ad66c97604e6220679b29089f060b5b730f7bc818b04edb39aba09a38/lo-drawings.docx) | 8943 | `c4e6188ad66c97604e6220679b29089f060b5b730f7bc818b04edb39aba09a38` |
| [lo-drawings.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/e3a27cf507261d13ef149e6cfed987fd851a4dc514608e01f6d41beb0bb17027/lo-drawings.odt) | 21377 | `e3a27cf507261d13ef149e6cfed987fd851a4dc514608e01f6d41beb0bb17027` |
| [lo-fields.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/32518ac3d7a2831a456905c37d9f369a51069e47a9c97bd630a4991bfcb28213/lo-fields.docx) | 6988 | `32518ac3d7a2831a456905c37d9f369a51069e47a9c97bd630a4991bfcb28213` |
| [lo-fields.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/0a5d5fa129b0d9bdad717ab9e4018e8b9d14a59aa6b83f0eb604b7155be5efe1/lo-fields.odt) | 12537 | `0a5d5fa129b0d9bdad717ab9e4018e8b9d14a59aa6b83f0eb604b7155be5efe1` |
| [lo-structure.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/d32d93fcef721faf2b29a458251a0b8a96692780dce46c84fdb78a9c701d02f6/lo-structure.docx) | 8815 | `d32d93fcef721faf2b29a458251a0b8a96692780dce46c84fdb78a9c701d02f6` |
| [lo-structure.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/a3e03e86acd4b3cd5c920580d13259e67f4321c3438f7ee1808a564c9790129a/lo-structure.odt) | 16030 | `a3e03e86acd4b3cd5c920580d13259e67f4321c3438f7ee1808a564c9790129a` |
| [sample.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/2d8d0ad24f42a12994d00a0305cbf2acef7df4504c20120f6787ffdfe0941c60/sample.docx) | 2882 | `2d8d0ad24f42a12994d00a0305cbf2acef7df4504c20120f6787ffdfe0941c60` |
| [sample.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/f3754e162a1e26858076e0595ccda4cf0bf2fdd39d368277f1fdd73377b50438/sample.odt) | 2059 | `f3754e162a1e26858076e0595ccda4cf0bf2fdd39d368277f1fdd73377b50438` |
| [xxe.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/c849645dee6b0535541b92f4ec53538c5cacd2ec5c2f30b2fe6342abc4f23f7f/xxe.docx) | 979 | `c849645dee6b0535541b92f4ec53538c5cacd2ec5c2f30b2fe6342abc4f23f7f` |
| [xxe.odt](https://code.tilo.so/api/packages/tilo/generic/source-evidence/7d7a1958c68075b77d4ab6399159250b064765eb22fb65da0660a96fe5d7780b/xxe.odt) | 739 | `7d7a1958c68075b77d4ab6399159250b064765eb22fb65da0660a96fe5d7780b` |
| [zipbomb.docx](https://code.tilo.so/api/packages/tilo/generic/source-evidence/106abd35af58c97a31404d56a89e2da46bcd5d16f065272d0564b4b157f7013a/zipbomb.docx) | 306784 | `106abd35af58c97a31404d56a89e2da46bcd5d16f065272d0564b4b157f7013a` |
