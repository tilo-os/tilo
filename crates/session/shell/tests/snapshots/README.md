# Reviewed shell references

`artifacts.json` pins the tab reorder and maximized decision states and the wheel disc
reference by public immutable URL, SHA-256 and byte length. Their original bytes
from `2a26be496c` are unchanged and retain the repository's license. Controlled
font licenses and provenance remain in `crates/sdk/ui-core/assets/fonts`.

These are required pixel comparisons, not illustrative screenshots. Tests
resolve verified cache entries even if an unreviewed PNG candidate exists in
the source directory. A missing offline reference, corrupt cache entry or
unavailable online artifact fails the comparison.

Prewarm the ignored cache before an offline run:

```sh
python3 tools/test-assets.py crates/session/shell/tests/snapshots/artifacts.json
TILO_REQUIRE_GPU=1 TILO_TEST_ASSETS_OFFLINE=1 cargo test --locked --offline -p shell --test it e2e_shell_flows::e2e_tab_drag_reorder
TILO_REQUIRE_GPU=1 TILO_TEST_ASSETS_OFFLINE=1 cargo test --locked --offline -p shell --test it sidebar_wheel_disc_snapshot::
```

`TILO_TEST_ASSET_CACHE` selects a prewarmed cache. Compilation requires no
reference download. `UPDATE_SNAPSHOTS=1` writes candidates in their original
`e2e/` or `wheel_disc/` directory, preserving review and update behavior without
overwriting the immutable cache. The visual-baseline workflow omits identical
archived candidates but retains changed and new images. After review, publish
approved replacements with `tools/upload_immutable_packages.sh`, verify public
readback and update the text pins. Generated PNGs stay out of Git.

The overview reference remains in Git while its current rendering mismatch is
investigated; its original comparison and tolerance are unchanged.
