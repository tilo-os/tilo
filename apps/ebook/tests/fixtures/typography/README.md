# Reader typography references

The original `serif-reader.png` and `math-reader.png` are the Linux references.
Windows uses the `-windows.png` references with the controlled font
catalog (`crates/sdk/ui-core/assets/fonts`). Both tests retain the same pixel comparison and
fail when an offscreen renderer is unavailable.

The Windows images were reviewed for text, line breaks, serif styling, formula
structure, and clipping. They are renderer outputs, not edited screenshots.
Keep baseline changes explicit; a successful render alone does not approve a
new reference image.

`artifacts.json` preserves all four original images by public immutable URL,
SHA-256 and byte length. The images retain the repository's license; font
licenses and provenance remain in `crates/sdk/ui-core/assets/fonts`. The Linux
references were introduced at `821fb29daeaec3698c60408b29722b37333c9a0d`;
the Windows references were retained at
`c865dc7526ee7ca4a3d4882f21b2561c56774476`. These original bytes are unchanged.

Prewarm the ignored cache before testing offline:

```sh
python3 tools/test-assets.py apps/ebook/tests/fixtures/typography/artifacts.json
TILO_TEST_ASSETS_OFFLINE=1 cargo test --locked --offline -p ebook --lib matches_typography_baseline
TILO_TEST_ASSETS_OFFLINE=1 cargo test --locked --offline -p ebook --lib matches_shared_typesetting_baseline
```

`TILO_TEST_ASSET_CACHE` selects a prewarmed cache. The resolver verifies every
read; a missing offline asset, corrupt file or unavailable online artifact
fails the test. Compilation needs no baseline download.

The update environment variables still write local PNG candidates for review.
The visual-baseline workflow omits identical archived candidates and preserves
changed or new images. Publish reviewed replacements with
`tools/upload_immutable_packages.sh`, verify public readback, then update the
text pins. Generated PNGs stay out of Git.
