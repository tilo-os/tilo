# Preparing immutable font assets

Font bytes live in immutable artifact storage. Source checkouts retain their
reviewed metadata, license notices and provenance. Native builds and packaging
materialize a verified catalog outside the source tree before using its fonts.

The artifacts.json inventory pins all 37 font files by size, SHA-256 and public
immutable URL. Each URL uses the full SHA-256 as both its package version and part
of its filename. The manifest remains authoritative for face selection, scope and
licensing. PROVENANCE.md records upstream versions, corresponding source,
copyright notices and license origins. Preparation copies that document and every
license notice byte for byte into the new catalog.

Each public font package version also carries a notices.txt companion containing
the original provenance document, its SPDX identifier and every mapped copyright
and license notice. The companion is beside the font at the same immutable
package version; replace the final URL filename with notices.txt to read it.

From the repository root, prewarm and materialize a catalog:

~~~sh
python3 tools/font-catalog.py prepare crates/sdk/ui-core/assets/fonts   target/prepared-fonts --scope fixtures
~~~

A prepared catalog is self-contained and can be staged without a network:

~~~sh
TILO_FONT_ASSETS_OFFLINE=1 python3 tools/font-catalog.py prepare   crates/sdk/ui-core/assets/fonts target/offline-fonts --scope fixtures
python3 tools/font-catalog.py stage target/offline-fonts   target/installed-fonts --scope os
~~~

The cache defaults to target/font-artifacts. Set TILO_FONT_ASSET_CACHE or pass
--cache to use a prewarmed cache elsewhere. --offline or
TILO_FONT_ASSETS_OFFLINE=1 rejects missing cache entries. Every use verifies the
file size and SHA-256; a corrupt entry fails instead of being replaced. Failed
preparation never publishes a partial destination. Destinations must be new.

The product scope continues to carry 35 fonts under OFL or LPPL; the OS carries
36, including New Computer Modern Regular under its GPL font exception; fixtures
carry 37. No font is subset, converted, renamed internally or replaced. Installed
catalogs retain all selected font bytes and notices and need no runtime fetch.

Cargo source builds prepare the fixture catalog through tilo-font-catalog's
build script. SDK packaging prepares the selected checkout's product catalog;
OS staging prepares its OS catalog. No application downloads fonts at runtime.
CI prewarms the digest cache and runs subsequent preparation offline.

For a self-contained offline SDK, set TILO_FONT_CATALOG_ROOT to a prepared
catalog whose manifest matches this checkout. The build verifies every selected
font and license before accepting it. Set CARGO_NET_OFFLINE=true or
TILO_FONT_ASSETS_OFFLINE=1 to require offline preparation. A fresh checkout with
neither a prewarmed cache nor a prepared catalog fails clearly.

Font regeneration scripts write candidates to target/font-candidates (or --out).
Review and publish changed bytes with corresponding manifest and artifact pins;
never write generated fonts into this metadata directory.
