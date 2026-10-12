# Canonical typography baselines

`artifacts.json` pins the six original Mesa llvmpipe PNGs by immutable public
URL, byte length and SHA-256. The images retain the repository's license;
rendered font provenance and licenses remain in `assets/fonts/LICENSES`.
The geometry and pixel comparisons read these same bytes through
`tools/test-assets.py`. Missing, corrupt or unavailable baselines fail the test.

Prewarm a checkout's ignored cache before an offline run:

```sh
python3 tools/test-assets.py crates/sdk/ui-core/tests/fixtures/typography/artifacts.json
TILO_TEST_ASSETS_OFFLINE=1 cargo test --locked --offline -p tilo-ui --test it typography_baseline_geometry::
```

`TILO_TEST_ASSET_CACHE` selects an already populated asset cache. Cache entries
are addressed by SHA-256 and verified on every read. No network is needed to
compile these tests or use a warmed cache. A fresh online test run fetches each
missing baseline from the existing artifact store; no credentials are needed.

The visual-baseline workflow still generates PNG candidates in this directory
for review. Identical archived bytes are omitted from its candidate bundle;
changed and new images remain in the bundle. After review, publish approved
images with `tools/upload_immutable_packages.sh`, verify public readback and
update `artifacts.json`. Commit only the text pins, never generated PNGs.
