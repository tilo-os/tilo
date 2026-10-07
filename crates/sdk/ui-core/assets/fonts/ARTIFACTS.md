# Preparing immutable font assets

The font files in this catalog are being migrated to immutable artifact storage.
This preparation change retains the source fonts while establishing a verified
cache and an offline materialization path for their build consumers.

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

Source font removal follows integration of preparation into all build and source
distribution paths, including native tests, SDK, Windows, macOS, Android, web and
the OS image. Until those paths are tested, the retained font pins are temporary
migration inventory.
