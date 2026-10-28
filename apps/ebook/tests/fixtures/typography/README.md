# Reader typography references

`serif-reader.png` and `math-reader.png` are the Linux references.
Windows uses the `-windows.png` references with the controlled font
catalog (`crates/sdk/ui-core/assets/fonts`). Both tests retain the same pixel comparison and
fail when an offscreen renderer is unavailable.

The Windows images were reviewed for text, line breaks, serif styling, formula
structure, and clipping. They are renderer outputs, not edited screenshots.
Keep baseline changes explicit; a successful render alone does not approve a
new reference image.

`artifacts.json` pins all four images by public immutable URL, SHA-256 and byte
length. The images retain the repository's license; font licenses and provenance
remain in `crates/sdk/ui-core/assets/fonts`. The original Linux references were
introduced at `821fb29daeaec3698c60408b29722b37333c9a0d`; the unchanged Windows
references were retained at `c865dc7526ee7ca4a3d4882f21b2561c56774476`.

The Linux references include the intentional area-coverage correction from
`6b530c69fc595b520e38583235266529c33316f1`: factoring the coverage expression
avoids floating-point cancellation and makes whole-pixel translations stable.
A controlled comparison on the pinned golden builder, with identical selected
font bytes, axes and llvmpipe driver, restored both original comparisons by
changing only the two WGSL coverage expressions back. Independent image review
confirmed unchanged glyphs, formula structure, wrapping, spacing and geometry;
the corrected antialiasing changes sparse edge pixels by at most 12 channel
steps. Only these two Linux references changed; comparison tolerances and
retained-pan coverage remain unchanged.

The current Linux references follow two intended reader changes: text is
set ragged by default (`9d637b1ff10259070b7ba6c0596aceb55045617c`) and the
progress slider ends in a percentage (`34b668de79b2681ba2ad3ce853129b80c20c1177`). Glyphs, formula
structure and the page frame are unchanged; they were rendered on the
builder image in `.config/visual-goldens-image`.

The Linux math reference follows the superscript fix in the shared math
layout: an exponent's baseline sits a fixed shift above its base's instead of
its cell resting on the line, so `c²` and `x²` are raised, the inline formula
sits on its sentence's baseline, and the lines after it no longer carry the
extra ascent the old placement reserved. Nothing else moved. The Windows math
reference predates that fix and was not re-rendered.

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
