# Settings image fixture

Original images are preserved byte for byte in the immutable package store. The existing repository [AGPL-3.0 license](https://code.tilo.so/api/packages/tilo/generic/source-evidence/0d96a4ff68ad6d4b6f1f30f713b18d5184912ba8dd389f86aa7710db079abcb0/license.txt) is archived unchanged alongside them; source and attribution remain at commit `b7ea536d18fef14cc95b3d94d400d43a601dad4e`. The original Store artwork introduction is commit `3f625b59dd1a8a4bd8e28eb1ef848d5a417ea603`.

The build resolves `artifacts.json` through `tools/test-assets.py`, verifies length and SHA-256, and copies the original bytes into `OUT_DIR`. The app embeds these bytes; runtime never downloads fixture images. Store catalogue URL keys keep their original spelling.

Prewarm with `python3 tools/test-assets.py apps/settings/shots/artifacts.json`. Set `TILO_TEST_ASSETS_OFFLINE=1` for offline builds using that cache. `TILO_TEST_ASSET_CACHE` overrides the cache location; `PYTHON` overrides the build-time interpreter. Missing or corrupt offline assets fail the build.

| Original image | Bytes | SHA-256 |
| --- | ---: | --- |
| [settings.png](https://code.tilo.so/api/packages/tilo/generic/source-evidence/bd5d23b61827b873b51daf0d84e09db582104b5a93c617c733a5c282191eb96f/settings.png) | 112053 | `bd5d23b61827b873b51daf0d84e09db582104b5a93c617c733a5c282191eb96f` |
